//! The binder: resolves the names of a parsed GQL query (stage 1a) into a [`BoundQuery`], the
//! input of the planner and the lowering.
//!
//! [`bind`] (or [`bind_with_parameters`] when the supplied parameter names are known) never
//! panics; every semantic error is a [`TraverserError::Gql`] of kind
//! [`GqlErrorKind::Semantic`] with the span of the offending text and, for a misspelt name, a
//! "did you mean" suggestion.
//!
//! # What it produces
//!
//! - **Bindings.** Every node and edge pattern element gets a [`BindingId`] and a
//!   [`BindingKind`]; ids follow the first appearance in the text. A named variable written
//!   again (in the same pattern, another pattern or a later `MATCH`) is the same binding, a join;
//!   anonymous elements get bindings without a name. Each binding collects the label expressions
//!   of all its occurrences (a conjunction).
//! - **Conjuncts.** Inline property maps (`{k: expr}` becomes `var.k = expr`), inline `WHERE` and
//!   the clause `WHERE` are split at their top-level `AND`s into [`Conjunct`]s, each tagged with
//!   the bindings it reads. A conjunct reading exactly one binding is listed on that binding
//!   ([`Binding::predicates`]); the others (zero or several bindings) are in
//!   [`BoundQuery::cross`]. Whether a conjunct is pushed down into `has()` is the lowering's
//!   decision (the plan's Phase 0 table), not the binder's.
//! - **Read flags (design D11).** [`Binding::read`] is set when an expression reads the binding
//!   away from its own position: a conjunct over several bindings, a `RETURN` column, an
//!   `ORDER BY` key. A conjunct over the binding alone is evaluated where the binding is the
//!   current element and does not set it. A repeated variable sets [`Binding::joined`]; whether
//!   the join needs a path label depends on the plan (continuing from the variable needs none,
//!   closing a cycle does), so the planner decides.
//! - **Columns.** In written order: the alias when given, else the expression text exactly as
//!   written (Neo4j's rule; the ISO naming rule of an unaliased column is still to be verified,
//!   so this is provisional). `RETURN *` comes first and expands to every named variable in the
//!   order of its first appearance (design D10).
//! - **`ORDER BY`** keys may name a `RETURN` alias (an alias shadows a variable of the same name,
//!   as in Neo4j) or read any variable in scope; a key whose text equals a column's expression
//!   text is that column. After `RETURN DISTINCT` a key may only read returned columns (Neo4j's
//!   rule).
//! - **`OFFSET` / `LIMIT`** are a non-negative integer literal or a parameter (Neo4j's rule); the
//!   lowering checks a parameter's value.
//! - **Parameters:** every `$name` reference with its span, in text order.
//!
//! # Scope
//!
//! A `MATCH` clause's inline maps, inline `WHERE` and `WHERE` see the variables of that clause and
//! of the earlier ones; `RETURN` and `ORDER BY` see all of them.
//!
//! # Errors (GQLSTATUS provisional, like the kind defaults)
//!
//! `42002` (the semantic default) for an undefined variable, a variable used as both a node and
//! an edge, an edge variable used twice in one `MATCH` (Neo4j: "Cannot use the same relationship
//! variable ... for multiple relationships"; trail semantics), `RETURN *` without a named
//! variable, a duplicate column name, an `ORDER BY` key reading a variable that `RETURN DISTINCT`
//! dropped, a duplicate key in a map, a parameter that was not supplied. `22G02` for a negative
//! `OFFSET` / `LIMIT` literal, `22G03` for one that is not an integer.

#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "used from the planner (T8) and the lowering (T9a) on"
    )
)]

use std::collections::HashMap;

use super::ast::{
    CallArgs, CountClause, EdgeDirection, Expr, ExprKind, Identifier, LabelExpr, Literal,
    MapLiteral, MatchClause, NodePattern, Query, SortDirection,
};
use super::lexer::Span;
use super::parser::close_to;
use crate::error::{GqlErrorBuilder, GqlErrorKind, TraverserError};

/// The index of a [`Binding`] in [`BoundQuery::bindings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct BindingId(pub(crate) usize);

/// Whether a binding holds a vertex or an edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindingKind {
    /// A node pattern `(..)`.
    Node,
    /// An edge pattern `-[..]->`.
    Edge,
}

/// One variable of the query (named or anonymous).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Binding {
    /// Its id (its index in [`BoundQuery::bindings`]).
    pub(crate) id: BindingId,
    /// Node or edge.
    pub(crate) kind: BindingKind,
    /// The variable as written at its first appearance; `None` for an anonymous element.
    pub(crate) name: Option<Identifier>,
    /// The span of every pattern element that binds it, in text order (more than one: a join).
    pub(crate) occurrences: Vec<Span>,
    /// The label expressions of its occurrences, all of which must hold.
    pub(crate) labels: Vec<LabelExpr>,
    /// Indexes into [`BoundQuery::conjuncts`] of the conjuncts reading this binding alone.
    pub(crate) predicates: Vec<usize>,
    /// Read by an expression away from its own position (module docs, D11).
    pub(crate) read: bool,
    /// The `MATCH` clause of its first appearance.
    pub(crate) clause: usize,
}

impl Binding {
    /// Whether the variable is written more than once (a join).
    pub(crate) fn joined(&self) -> bool {
        self.occurrences.len() > 1
    }
}

/// A bound `MATCH` clause.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoundMatch {
    /// The comma-separated path patterns in order.
    pub(crate) paths: Vec<BoundPath>,
    /// The clause.
    pub(crate) span: Span,
}

/// A bound path pattern.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoundPath {
    /// The first node.
    pub(crate) start: BindingId,
    /// The edges, each with the node it leads to.
    pub(crate) steps: Vec<BoundStep>,
    /// The pattern.
    pub(crate) span: Span,
}

/// One edge of a bound path and the node after it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BoundStep {
    /// The edge binding.
    pub(crate) edge: BindingId,
    /// The direction as written.
    pub(crate) direction: EdgeDirection,
    /// The node binding it leads to.
    pub(crate) node: BindingId,
}

/// Where a conjunct was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConjunctOrigin {
    /// An entry of an inline property map `{k: expr}`.
    InlineProperty,
    /// An inline `WHERE` of a node or edge pattern.
    InlineWhere,
    /// The `WHERE` of a `MATCH` clause.
    Where,
}

/// The condition of a [`Conjunct`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Predicate {
    /// `binding.key = value`, from an inline property map (the binding may be anonymous).
    Property {
        /// The element whose property is compared.
        binding: BindingId,
        /// The property key.
        key: Identifier,
        /// The value it must equal.
        value: Expr,
    },
    /// A `WHERE` conjunct (no top-level `AND`).
    Expr(Expr),
}

/// One top-level `AND` conjunct of the query's filters; a row passes only when every conjunct
/// is `TRUE`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Conjunct {
    /// The condition.
    pub(crate) predicate: Predicate,
    /// The bindings it reads, sorted, without duplicates.
    pub(crate) bindings: Vec<BindingId>,
    /// Where it was written.
    pub(crate) origin: ConjunctOrigin,
    /// The `MATCH` clause it belongs to.
    pub(crate) clause: usize,
    /// Its text (`k: expr` for an inline property).
    pub(crate) span: Span,
}

/// One `RETURN` column.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Column {
    /// The column name: the alias, else the expression text as written.
    pub(crate) name: String,
    /// The expression (a variable reference for a column of `RETURN *`).
    pub(crate) expr: Expr,
    /// The binding when the expression is a bare variable.
    pub(crate) variable: Option<BindingId>,
    /// The bindings it reads, sorted, without duplicates.
    pub(crate) bindings: Vec<BindingId>,
    /// Whether the name comes from `AS`.
    pub(crate) aliased: bool,
    /// The item (the `*` for a column of `RETURN *`).
    pub(crate) span: Span,
}

/// One `ORDER BY` key.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SortKey {
    /// The key expression.
    pub(crate) expr: Expr,
    /// The direction.
    pub(crate) direction: SortDirection,
    /// The column the whole key is (an alias, or the same text as a column's expression).
    pub(crate) column: Option<usize>,
    /// The bindings the key reads (through a column too), sorted, without duplicates.
    pub(crate) bindings: Vec<BindingId>,
    /// The aliased columns the key names inside its expression (identifiers that resolve to an
    /// alias rather than a variable), sorted, without duplicates.
    pub(crate) aliases: Vec<usize>,
    /// The item.
    pub(crate) span: Span,
}

/// The value of `OFFSET` / `LIMIT`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Count {
    /// A non-negative integer literal.
    Literal(u64),
    /// `$name`: the lowering checks the supplied value.
    Parameter {
        /// The parameter name.
        name: String,
        /// The `$name` text.
        span: Span,
    },
}

/// One `$name` reference.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ParameterRef {
    /// The name without `$`.
    pub(crate) name: String,
    /// The `$name` text.
    pub(crate) span: Span,
}

/// A query with every name resolved (module docs).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoundQuery {
    /// Every binding, by [`BindingId`], in order of first appearance.
    pub(crate) bindings: Vec<Binding>,
    /// The `MATCH` clauses.
    pub(crate) matches: Vec<BoundMatch>,
    /// Every conjunct of every filter, in text order per clause.
    pub(crate) conjuncts: Vec<Conjunct>,
    /// Indexes into [`Self::conjuncts`] of those reading zero or several bindings.
    pub(crate) cross: Vec<usize>,
    /// The `RETURN` columns in order.
    pub(crate) columns: Vec<Column>,
    /// `RETURN DISTINCT`.
    pub(crate) distinct: bool,
    /// The `ORDER BY` keys in priority order.
    pub(crate) order_by: Vec<SortKey>,
    /// `OFFSET` / `SKIP`.
    pub(crate) offset: Option<Count>,
    /// `LIMIT`.
    pub(crate) limit: Option<Count>,
    /// Every parameter reference in text order.
    pub(crate) parameters: Vec<ParameterRef>,
}

impl BoundQuery {
    /// The binding `id`.
    pub(crate) fn binding(&self, id: BindingId) -> Option<&Binding> {
        self.bindings.get(id.0)
    }

    /// The binding of the variable `name`.
    pub(crate) fn lookup(&self, name: &str) -> Option<BindingId> {
        self.bindings
            .iter()
            .find(|b| b.name.as_ref().is_some_and(|n| n.name == name))
            .map(|b| b.id)
    }
}

/// Binds `query` without checking parameter values (the caller checks them later).
pub(crate) fn bind(query: &Query) -> Result<BoundQuery, TraverserError> {
    Binder::new(query)
        .run(None)
        .map_err(|e| e.into_error(&query.source))
}

/// Binds `query` and checks that every `$name` it reads is among `supplied` (a missing one is a
/// semantic error suggesting the closest supplied name).
pub(crate) fn bind_with_parameters<'a>(
    query: &Query,
    supplied: impl IntoIterator<Item = &'a str>,
) -> Result<BoundQuery, TraverserError> {
    let supplied: Vec<&str> = supplied.into_iter().collect();
    Binder::new(query)
        .run(Some(&supplied))
        .map_err(|e| e.into_error(&query.source))
}

type BResult<T> = Result<T, Box<GqlErrorBuilder>>;

fn semantic(span: Span, message: impl Into<String>) -> GqlErrorBuilder {
    GqlErrorBuilder::new(GqlErrorKind::Semantic, span.start..span.end, message)
}

/// The closest of `candidates` to `word` (a case-only difference is the closest).
fn suggest<'c>(word: &str, candidates: impl IntoIterator<Item = &'c str>) -> Option<&'c str> {
    candidates
        .into_iter()
        .filter_map(|c| {
            if c != word && c.eq_ignore_ascii_case(word) {
                Some((0, c))
            } else {
                close_to(word, c).map(|d| (d, c))
            }
        })
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

/// What the identifiers of an expression may name.
#[derive(Clone, Copy)]
struct Scope<'s> {
    /// Variables first bound in this `MATCH` clause or an earlier one.
    clause: usize,
    /// The aliased `RETURN` columns (`ORDER BY` only): name and column index.
    aliases: &'s [(String, usize)],
}

/// What an expression reads.
#[derive(Default)]
struct Refs {
    bindings: Vec<BindingId>,
    aliases: Vec<usize>,
}

impl Refs {
    fn finish(mut self) -> (Vec<BindingId>, Vec<usize>) {
        self.bindings.sort_unstable();
        self.bindings.dedup();
        self.aliases.sort_unstable();
        self.aliases.dedup();
        (self.bindings, self.aliases)
    }
}

struct Binder<'q> {
    query: &'q Query,
    bindings: Vec<Binding>,
    names: HashMap<String, BindingId>,
    conjuncts: Vec<Conjunct>,
    parameters: Vec<ParameterRef>,
}

impl<'q> Binder<'q> {
    fn new(query: &'q Query) -> Self {
        Binder {
            query,
            bindings: Vec::new(),
            names: HashMap::new(),
            conjuncts: Vec::new(),
            parameters: Vec::new(),
        }
    }

    fn run(mut self, supplied: Option<&[&str]>) -> BResult<BoundQuery> {
        let mut matches = Vec::with_capacity(self.query.matches.len());
        // Declare every element first, so a filter naming a variable of a later clause is told
        // so; then the filters, clause by clause, in text order.
        for (clause, m) in self.query.matches.iter().enumerate() {
            matches.push(self.declare_clause(clause, m)?);
        }
        for (clause, (m, bound)) in self.query.matches.iter().zip(&matches).enumerate() {
            self.clause_filters(clause, m, bound)?;
        }
        let columns = self.columns()?;
        let order_by = self.order_by(&columns)?;
        let offset = self.count(self.query.offset.as_ref(), "OFFSET")?;
        let limit = self.count(self.query.limit.as_ref(), "LIMIT")?;
        self.parameters.sort_by_key(|p| p.span.start);
        if let Some(supplied) = supplied {
            self.check_parameters(supplied)?;
        }
        let mut cross = Vec::new();
        for (index, conjunct) in self.conjuncts.iter().enumerate() {
            if let [only] = conjunct.bindings[..] {
                if let Some(binding) = self.bindings.get_mut(only.0) {
                    binding.predicates.push(index);
                }
            } else {
                cross.push(index);
            }
        }
        Ok(BoundQuery {
            bindings: self.bindings,
            matches,
            conjuncts: self.conjuncts,
            cross,
            columns,
            distinct: self.query.ret.distinct,
            order_by,
            offset,
            limit,
            parameters: self.parameters,
        })
    }

    // ----- patterns -----

    fn declare_clause(&mut self, clause: usize, m: &MatchClause) -> BResult<BoundMatch> {
        let mut edges_here: Vec<BindingId> = Vec::new();
        let mut paths = Vec::with_capacity(m.patterns.len());
        for path in &m.patterns {
            let start = self.node(clause, &path.start)?;
            let mut steps = Vec::with_capacity(path.steps.len());
            for step in &path.steps {
                let e = &step.edge;
                let edge = self.declare(
                    clause,
                    e.variable.as_ref(),
                    BindingKind::Edge,
                    e.span,
                    e.labels.as_ref(),
                )?;
                if let Some(var) = &e.variable {
                    if edges_here.contains(&edge) {
                        return Err(semantic(
                            var.span,
                            format!(
                                "the edge variable `{}` is used for more than one edge in the \
                                 same MATCH (an edge is bound once per MATCH)",
                                var.name
                            ),
                        )
                        .alternative(format!(
                            "give the second edge its own variable, or put the second pattern \
                             in a MATCH clause of its own to join on the same edge `{}`",
                            var.name
                        ))
                        .into());
                    }
                    edges_here.push(edge);
                }
                let node = self.node(clause, &step.node)?;
                steps.push(BoundStep {
                    edge,
                    direction: e.direction,
                    node,
                });
            }
            paths.push(BoundPath {
                start,
                steps,
                span: path.span,
            });
        }
        Ok(BoundMatch {
            paths,
            span: m.span,
        })
    }

    fn clause_filters(
        &mut self,
        clause: usize,
        m: &MatchClause,
        bound: &BoundMatch,
    ) -> BResult<()> {
        let paths = &bound.paths;
        let scope = Scope {
            clause,
            aliases: &[],
        };
        for (path, bound) in m.patterns.iter().zip(paths) {
            self.element_filters(
                scope,
                bound.start,
                path.start.properties.as_ref(),
                path.start.where_clause.as_ref(),
            )?;
            for (step, bound_step) in path.steps.iter().zip(&bound.steps) {
                self.element_filters(
                    scope,
                    bound_step.edge,
                    step.edge.properties.as_ref(),
                    step.edge.where_clause.as_ref(),
                )?;
                self.element_filters(
                    scope,
                    bound_step.node,
                    step.node.properties.as_ref(),
                    step.node.where_clause.as_ref(),
                )?;
            }
        }
        if let Some(where_clause) = &m.where_clause {
            self.conjuncts_of(scope, where_clause, ConjunctOrigin::Where)?;
        }
        Ok(())
    }

    fn node(&mut self, clause: usize, node: &NodePattern) -> BResult<BindingId> {
        self.declare(
            clause,
            node.variable.as_ref(),
            BindingKind::Node,
            node.span,
            node.labels.as_ref(),
        )
    }

    /// The binding of a pattern element: a new one, or the existing one of a repeated variable.
    fn declare(
        &mut self,
        clause: usize,
        variable: Option<&Identifier>,
        kind: BindingKind,
        span: Span,
        labels: Option<&LabelExpr>,
    ) -> BResult<BindingId> {
        let existing = variable.and_then(|v| self.names.get(&v.name).copied());
        let id = match existing {
            Some(id) => {
                let binding = self.bindings.get_mut(id.0).ok_or_else(|| {
                    semantic(
                        span,
                        "internal error: a binding id out of range in the binder",
                    )
                })?;
                if binding.kind != kind {
                    let (here, there) = match kind {
                        BindingKind::Node => ("a node", "an edge"),
                        BindingKind::Edge => ("an edge", "a node"),
                    };
                    let var = variable.map_or(span, |v| v.span);
                    let name = variable.map_or("", |v| v.name.as_str());
                    return Err(semantic(
                        var,
                        format!(
                            "the variable `{name}` is used as {here} here but is already bound \
                             to {there}; a variable is either a node or an edge"
                        ),
                    )
                    .into());
                }
                binding.occurrences.push(span);
                if let Some(labels) = labels {
                    binding.labels.push(labels.clone());
                }
                id
            }
            None => {
                let id = BindingId(self.bindings.len());
                if let Some(v) = variable {
                    self.names.insert(v.name.clone(), id);
                }
                self.bindings.push(Binding {
                    id,
                    kind,
                    name: variable.cloned(),
                    occurrences: vec![span],
                    labels: labels.into_iter().cloned().collect(),
                    predicates: Vec::new(),
                    read: false,
                    clause,
                });
                id
            }
        };
        Ok(id)
    }

    /// The inline property map and inline `WHERE` of one pattern element.
    fn element_filters(
        &mut self,
        scope: Scope<'_>,
        binding: BindingId,
        properties: Option<&MapLiteral>,
        where_clause: Option<&Expr>,
    ) -> BResult<()> {
        if let Some(map) = properties {
            check_map_keys(map)?;
            for entry in &map.entries {
                let mut refs = Refs {
                    bindings: vec![binding],
                    aliases: Vec::new(),
                };
                self.resolve(scope, &entry.value, &mut refs)?;
                let (bindings, _) = refs.finish();
                self.push_conjunct(Conjunct {
                    predicate: Predicate::Property {
                        binding,
                        key: entry.key.clone(),
                        value: entry.value.clone(),
                    },
                    bindings,
                    origin: ConjunctOrigin::InlineProperty,
                    clause: scope.clause,
                    span: entry.key.span.to(entry.value.span),
                });
            }
        }
        if let Some(where_clause) = where_clause {
            self.conjuncts_of(scope, where_clause, ConjunctOrigin::InlineWhere)?;
        }
        Ok(())
    }

    /// Splits `expr` at its top-level `AND`s and records each conjunct.
    fn conjuncts_of(
        &mut self,
        scope: Scope<'_>,
        expr: &Expr,
        origin: ConjunctOrigin,
    ) -> BResult<()> {
        for conjunct in split_and(expr) {
            let mut refs = Refs::default();
            self.resolve(scope, conjunct, &mut refs)?;
            let (bindings, _) = refs.finish();
            self.push_conjunct(Conjunct {
                predicate: Predicate::Expr(conjunct.clone()),
                bindings,
                origin,
                clause: scope.clause,
                span: conjunct.span,
            });
        }
        Ok(())
    }

    fn push_conjunct(&mut self, conjunct: Conjunct) {
        if conjunct.bindings.len() > 1 {
            self.mark_read(&conjunct.bindings);
        }
        self.conjuncts.push(conjunct);
    }

    fn mark_read(&mut self, ids: &[BindingId]) {
        for id in ids {
            if let Some(binding) = self.bindings.get_mut(id.0) {
                binding.read = true;
            }
        }
    }

    // ----- expressions -----

    /// Resolves every name in `expr` (recursion bounded by the parser's `MAX_HEIGHT`), records
    /// its parameters and checks its map literals.
    fn resolve(&mut self, scope: Scope<'_>, expr: &Expr, refs: &mut Refs) -> BResult<()> {
        match &expr.kind {
            ExprKind::Literal(_) => {}
            ExprKind::Parameter(name) => self.parameters.push(ParameterRef {
                name: name.clone(),
                span: expr.span,
            }),
            ExprKind::Variable(var) => self.variable(scope, var, refs)?,
            ExprKind::List(items) => {
                for item in items {
                    self.resolve(scope, item, refs)?;
                }
            }
            ExprKind::Map(map) => {
                check_map_keys(map)?;
                for entry in &map.entries {
                    self.resolve(scope, &entry.value, refs)?;
                }
            }
            ExprKind::Property { target, .. } => self.resolve(scope, target, refs)?,
            ExprKind::Unary { operand, .. }
            | ExprKind::IsNull { operand, .. }
            | ExprKind::IsLabeled { operand, .. } => self.resolve(scope, operand, refs)?,
            ExprKind::Binary { left, right, .. } => {
                self.resolve(scope, left, refs)?;
                self.resolve(scope, right, refs)?;
            }
            ExprKind::Case(case) => {
                if let Some(operand) = &case.operand {
                    self.resolve(scope, operand, refs)?;
                }
                for arm in &case.whens {
                    self.resolve(scope, &arm.when, refs)?;
                    self.resolve(scope, &arm.then, refs)?;
                }
                if let Some(else_result) = &case.else_result {
                    self.resolve(scope, else_result, refs)?;
                }
            }
            ExprKind::FunctionCall { args, .. } => match args {
                CallArgs::Star(_) => {}
                CallArgs::Exprs(args) => {
                    for arg in args {
                        self.resolve(scope, arg, refs)?;
                    }
                }
            },
        }
        Ok(())
    }

    fn variable(&self, scope: Scope<'_>, var: &Identifier, refs: &mut Refs) -> BResult<()> {
        if let Some((_, column)) = scope.aliases.iter().find(|(name, _)| *name == var.name) {
            refs.aliases.push(*column);
            return Ok(());
        }
        let found = self
            .names
            .get(&var.name)
            .and_then(|id| self.bindings.get(id.0));
        match found {
            Some(binding) if binding.clause <= scope.clause => {
                refs.bindings.push(binding.id);
                Ok(())
            }
            Some(_) => Err(semantic(
                var.span,
                format!(
                    "the variable `{}` is not defined here: it is bound by a later MATCH clause",
                    var.name
                ),
            )
            .alternative("move the condition to the WHERE of that later MATCH clause")
            .into()),
            None => {
                let in_scope = self
                    .bindings
                    .iter()
                    .filter(|b| b.clause <= scope.clause)
                    .filter_map(|b| b.name.as_ref().map(|n| n.name.as_str()))
                    .chain(scope.aliases.iter().map(|(name, _)| name.as_str()));
                let mut error = semantic(
                    var.span,
                    format!("the variable `{}` is not defined", var.name),
                );
                if let Some(close) = suggest(&var.name, in_scope) {
                    error = error.suggestion(close);
                }
                Err(Box::new(error))
            }
        }
    }

    // ----- result -----

    fn columns(&mut self) -> BResult<Vec<Column>> {
        let ret = &self.query.ret;
        let mut columns = Vec::new();
        if let Some(star) = ret.star {
            let named: Vec<(BindingId, Identifier)> = self
                .bindings
                .iter()
                .filter_map(|b| b.name.clone().map(|n| (b.id, n)))
                .collect();
            if named.is_empty() {
                return Err(semantic(
                    star,
                    "RETURN * needs at least one named variable, and this query has none",
                )
                .alternative(
                    "name an element of the pattern, as in MATCH (n) RETURN *, or list the \
                     columns: RETURN count AS c",
                )
                .into());
            }
            for (id, name) in named {
                columns.push(Column {
                    name: name.name.clone(),
                    expr: Expr {
                        kind: ExprKind::Variable(name.clone()),
                        span: name.span,
                    },
                    variable: Some(id),
                    bindings: vec![id],
                    aliased: false,
                    span: star,
                });
            }
        }
        let last = self.query.matches.len().saturating_sub(1);
        let scope = Scope {
            clause: last,
            aliases: &[],
        };
        for item in &ret.items {
            let mut refs = Refs::default();
            self.resolve(scope, &item.expr, &mut refs)?;
            let (bindings, _) = refs.finish();
            let variable = match &item.expr.kind {
                ExprKind::Variable(var) => self.names.get(&var.name).copied(),
                _ => None,
            };
            let (name, aliased) = match &item.alias {
                Some(alias) => (alias.name.clone(), true),
                None => (self.query.text(item.expr.span).to_string(), false),
            };
            if columns.iter().any(|c| c.name == name) {
                let at = item.alias.as_ref().map_or(item.expr.span, |a| a.span);
                return Err(semantic(
                    at,
                    format!("the column name `{name}` is used twice in RETURN"),
                )
                .alternative(format!(
                    "give one of the columns another name with AS, as in RETURN ... AS {name}2"
                ))
                .into());
            }
            columns.push(Column {
                name,
                expr: item.expr.clone(),
                variable,
                bindings,
                aliased,
                span: item.span,
            });
        }
        for column in &columns {
            let ids = column.bindings.clone();
            self.mark_read(&ids);
        }
        Ok(columns)
    }

    fn order_by(&mut self, columns: &[Column]) -> BResult<Vec<SortKey>> {
        let Some(order_by) = &self.query.order_by else {
            return Ok(Vec::new());
        };
        let aliases: Vec<(String, usize)> = columns
            .iter()
            .enumerate()
            .filter(|(_, c)| c.aliased)
            .map(|(i, c)| (c.name.clone(), i))
            .collect();
        let scope = Scope {
            clause: self.query.matches.len().saturating_sub(1),
            aliases: &aliases,
        };
        let mut keys = Vec::with_capacity(order_by.items.len());
        for item in &order_by.items {
            let mut refs = Refs::default();
            self.resolve(scope, &item.expr, &mut refs)?;
            let text = self.query.text(item.expr.span);
            let column = match (&item.expr.kind, refs.aliases.as_slice()) {
                (ExprKind::Variable(_), [alias]) => Some(*alias),
                _ => columns
                    .iter()
                    .position(|c| self.query.text(c.expr.span) == text && !text.is_empty()),
            };
            if self.query.ret.distinct && column.is_none() {
                // Read by name: each must be a returned variable (reads through an alias are).
                let dropped = refs
                    .bindings
                    .iter()
                    .find(|id| !columns.iter().any(|c| c.variable == Some(**id)));
                if let Some(id) = dropped {
                    let name = self
                        .bindings
                        .get(id.0)
                        .and_then(|b| b.name.as_ref())
                        .map_or("", |n| n.name.as_str());
                    return Err(semantic(
                        item.expr.span,
                        format!(
                            "after RETURN DISTINCT, ORDER BY can only use the returned columns, \
                             and `{name}` is not one of them"
                        ),
                    )
                    .alternative(format!(
                        "return the sort key too (RETURN DISTINCT ..., {text}) and order by it, \
                         or drop DISTINCT"
                    ))
                    .into());
                }
            }
            for alias in &refs.aliases {
                if let Some(c) = columns.get(*alias) {
                    refs.bindings.extend_from_slice(&c.bindings);
                }
            }
            let (bindings, aliases_read) = refs.finish();
            self.mark_read(&bindings);
            keys.push(SortKey {
                expr: item.expr.clone(),
                direction: item.direction,
                column,
                bindings,
                aliases: aliases_read,
                span: item.span,
            });
        }
        Ok(keys)
    }

    fn count(&mut self, clause: Option<&CountClause>, keyword: &str) -> BResult<Option<Count>> {
        let Some(clause) = clause else {
            return Ok(None);
        };
        let value = &clause.value;
        let refused = |code: &'static str, what: &str| {
            Err(semantic(
                value.span,
                format!("{keyword} must be a non-negative integer or a parameter, not {what}"),
            )
            .gqlstatus(code)
            .alternative(format!("{keyword} 10 or {keyword} $n"))
            .into())
        };
        let count = match &value.kind {
            ExprKind::Literal(Literal::Integer(n)) => match u64::try_from(*n) {
                Ok(n) => Count::Literal(n),
                Err(_) => return refused("22G02", "a negative number"),
            },
            ExprKind::Parameter(name) => {
                self.parameters.push(ParameterRef {
                    name: name.clone(),
                    span: value.span,
                });
                Count::Parameter {
                    name: name.clone(),
                    span: value.span,
                }
            }
            ExprKind::Literal(Literal::Float(_)) => return refused("22G03", "a float"),
            ExprKind::Literal(_) => return refused("22G03", "this literal"),
            _ => return refused("22G03", "an expression"),
        };
        Ok(Some(count))
    }

    fn check_parameters(&self, supplied: &[&str]) -> BResult<()> {
        let missing = self
            .parameters
            .iter()
            .find(|p| !supplied.contains(&p.name.as_str()));
        let Some(missing) = missing else {
            return Ok(());
        };
        let mut error = semantic(
            missing.span,
            format!("the parameter `${}` has no value", missing.name),
        )
        .alternative(format!(
            "pass it in the parameters of gql(), as in g.gql(query, #{{{}: value}})",
            missing.name
        ));
        if let Some(close) = suggest(&missing.name, supplied.iter().copied()) {
            error = error.suggestion(format!("${close}"));
        }
        Err(Box::new(error))
    }
}

/// The top-level `AND` conjuncts of `expr`, left to right (iterative: an `AND` chain may be as
/// tall as `MAX_HEIGHT`).
fn split_and(expr: &Expr) -> Vec<&Expr> {
    let mut out = Vec::new();
    let mut stack = vec![expr];
    while let Some(e) = stack.pop() {
        match &e.kind {
            ExprKind::Binary {
                op: super::ast::BinaryOp::And,
                left,
                right,
            } => {
                stack.push(right);
                stack.push(left);
            }
            _ => out.push(e),
        }
    }
    out
}

/// A key written twice in one map is an error.
fn check_map_keys(map: &MapLiteral) -> BResult<()> {
    for (i, entry) in map.entries.iter().enumerate() {
        let earlier = map.entries.get(..i).unwrap_or(&[]);
        if earlier.iter().any(|e| e.key.name == entry.key.name) {
            return Err(semantic(
                entry.key.span,
                format!("the key `{}` appears twice in this map", entry.key.name),
            )
            .into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
