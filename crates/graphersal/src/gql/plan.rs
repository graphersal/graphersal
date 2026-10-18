//! The logical plan of a bound GQL query (stage 1a): what the planner decided, in the order the
//! lowering emits it.
//!
//! A [`LogicalPlan`] has two parts:
//!
//! - **the match pipeline** ([`LogicalPlan::ops`]): for every connected component of the pattern
//!   graph (node bindings as vertices, edge bindings as edges, across all `MATCH` clauses, since
//!   stage 1a has no `OPTIONAL MATCH`) a [`Op::Scan`] (or, from phase 2 on, an [`Op::Anchor`])
//!   followed by one [`Op::Expand`] per pattern edge in walk order, an [`Op::Join`] after an
//!   expansion that reaches a node bound earlier (a cycle closing or a node repeated across
//!   patterns), and [`Op::Filter`]s of the conjuncts over several bindings (or none), each placed
//!   right after the operation that binds the last binding it reads. Several components follow
//!   one another: the second one starts with a second scan, a Cartesian product (correct but not
//!   optimized, Phase 0 outcome 2);
//! - **the result pipeline** ([`LogicalPlan::result`]): [`ResultOp::Project`], then
//!   [`ResultOp::Distinct`], [`ResultOp::Order`], [`ResultOp::Offset`], [`ResultOp::Limit`] as
//!   far as the query has them.
//!
//! The conjuncts over one binding are not operations of their own: they belong to the element
//! that binds it ([`ElementFilters`]: the scan's node, an expansion's edge or new node), split
//! into those the lowering may push into `has()` ([`Pushdown`], the Phase 0 Q5 table, see
//! [`classify`]) and the residual ones for `gql:filter`. A repeated binding gets its filters
//! (and its label expressions) once, where it is bound first.
//!
//! **Path labels (design D11).** [`LogicalPlan::labelling`] records per binding why it needs a
//! path label (`as()`): read by a later expression ([`Binding::read`](super::bind::Binding)), the
//! target of a join check, the target of a trail check, or the origin of an expansion that does
//! not continue from the current element (a branch, lowered as `select(x)`). Nothing else is
//! labelled, so bulk merging is not lost to bookkeeping the query never reads.
//!
//! **Phase 2 seam.** The planner takes an optional anchor binding (always `None` in stage 1a):
//! the component holding it starts with [`Op::Anchor`] (the incoming traverser) instead of a
//! scan. [`ANCHOR_VARIABLE`] is the reserved name of that anchor.

#![cfg_attr(
    not(test),
    allow(dead_code, reason = "used from the lowering (T9a) on")
)]

use std::collections::BTreeSet;

use super::ast::{BinaryOp, EdgeDirection, Expr, ExprKind, LabelExpr, LabelExprKind, Literal};
use super::bind::{BindingId, BoundQuery, Conjunct, Count, Predicate};
use super::lexer::Span;

/// The reserved variable name of the phase 2 anchor (the incoming traverser of a mid-traversal
/// `gql()`): the empty string. No valid query can write it: a plain identifier is never empty
/// and an empty back-quoted identifier is a lexical error, so the anchor never collides with a
/// variable of the query.
pub(crate) const ANCHOR_VARIABLE: &str = "";

/// The planned query (module docs).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LogicalPlan {
    /// The match pipeline: every component in emission order.
    pub(crate) ops: Vec<Op>,
    /// The result pipeline.
    pub(crate) result: Vec<ResultOp>,
    /// Why each binding needs a path label, by [`BindingId`].
    pub(crate) labelling: Vec<Labelling>,
    /// The push-down form of each conjunct, by conjunct index; `None` for a conjunct that is not
    /// pushable or does not read exactly one binding.
    pub(crate) pushdown: Vec<Option<Pushdown>>,
    /// The phase 2 anchor binding (always `None` in stage 1a).
    pub(crate) anchor: Option<BindingId>,
}

impl LogicalPlan {
    /// Whether the binding `id` needs a path label (`as()`).
    pub(crate) fn must_label(&self, id: BindingId) -> bool {
        self.labelling.get(id.0).is_some_and(Labelling::needed)
    }

    /// The bindings that need a path label, in id order.
    pub(crate) fn labelled(&self) -> Vec<BindingId> {
        (0..self.labelling.len())
            .map(BindingId)
            .filter(|&id| self.must_label(id))
            .collect()
    }
}

/// One operation of the match pipeline.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Op {
    /// The start of a component: every vertex matching the binding's labels and filters.
    Scan(Scan),
    /// The start of the anchor's component (phase 2): the incoming traverser, checked against
    /// the binding's labels and filters. Never produced in stage 1a.
    Anchor(Scan),
    /// One pattern edge, walked from a bound node to the other end.
    Expand(Expand),
    /// The current element (just reached by the preceding [`Op::Expand`]) must equal the
    /// element bound earlier to `binding` (lowered as `where(P.eq(x))`).
    Join {
        /// The node bound earlier.
        binding: BindingId,
    },
    /// Conjunct indexes over zero or several bindings, every one of them bound here.
    Filter(Vec<usize>),
}

/// A scan (or the anchor) of a component's start node.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Scan {
    /// The node binding.
    pub(crate) binding: BindingId,
    /// The label disjunction the lowering emits first, so that it folds into `v(labels: ..)`
    /// (and an index can serve it): the first `&` factor of the binding's label expressions that
    /// is a plain label or a disjunction of plain labels. `None`: no such factor.
    pub(crate) scan_labels: Option<Vec<String>>,
    /// The node's own conjuncts.
    pub(crate) filters: ElementFilters,
}

/// One walked pattern edge.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Expand {
    /// The bound node the walk starts from.
    pub(crate) from: BindingId,
    /// Whether `from` is the current element (else the lowering first returns to it with
    /// `select(from)`, and `from` is labelled).
    pub(crate) from_current: bool,
    /// The edge binding.
    pub(crate) edge: BindingId,
    /// The direction as seen from `from` (`Right`: `from` is the edge's source).
    pub(crate) direction: EdgeDirection,
    /// The edge binding was bound earlier (the same edge variable in an earlier `MATCH`): the
    /// edge must equal it, checked on the edge traverser (lowered as `where(P.eq(e))`).
    pub(crate) edge_join: bool,
    /// Earlier edges of the same `MATCH` clause this edge must differ from (trail semantics),
    /// checked on the edge traverser right after the edge step; edges whose label sets cannot
    /// overlap are left out (Phase 0 Q8).
    pub(crate) trail: Vec<BindingId>,
    /// The edge's own conjuncts (empty when `edge_join`).
    pub(crate) edge_filters: ElementFilters,
    /// The node reached.
    pub(crate) to: BindingId,
    /// `to` was bound earlier: an [`Op::Join`] follows, and `to_filters` is empty.
    pub(crate) to_bound: bool,
    /// The new node's own conjuncts.
    pub(crate) to_filters: ElementFilters,
}

/// The conjuncts over one binding, applied where the binding is the current element.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ElementFilters {
    /// Conjunct indexes the lowering pushes into `has()`, most selective first (an indexed key
    /// first, then equality, `IN`, text and range forms, `IS NOT NULL`).
    pub(crate) pushed: Vec<usize>,
    /// Conjunct indexes for `gql:filter`, in text order.
    pub(crate) residual: Vec<usize>,
}

impl ElementFilters {
    /// Whether there is no conjunct at all.
    pub(crate) fn is_empty(&self) -> bool {
        self.pushed.is_empty() && self.residual.is_empty()
    }
}

/// One operation of the result pipeline (the details are in the [`BoundQuery`]).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ResultOp {
    /// `RETURN`: one row map of [`BoundQuery::columns`].
    Project,
    /// `RETURN DISTINCT`.
    Distinct,
    /// `ORDER BY` [`BoundQuery::order_by`].
    Order,
    /// `OFFSET` / `SKIP`.
    Offset(Count),
    /// `LIMIT`.
    Limit(Count),
}

/// Why a binding needs a path label (all `false`: it needs none, D11).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Labelling {
    /// Read by a later expression (filter over several bindings, `RETURN`, `ORDER BY`).
    pub(crate) read: bool,
    /// The target of an [`Op::Join`] or an [`Expand::edge_join`].
    pub(crate) join: bool,
    /// Listed in a later edge's [`Expand::trail`].
    pub(crate) trail: bool,
    /// The origin of an expansion that does not continue from the current element.
    pub(crate) branch: bool,
}

impl Labelling {
    /// Whether any reason holds.
    pub(crate) fn needed(&self) -> bool {
        self.read || self.join || self.trail || self.branch
    }
}

// ----- push-down classification (Phase 0 Q5) -----

/// A conjunct over one binding in a form `has()` evaluates exactly like GQL's three-valued
/// `WHERE` at a top-level `AND` (Phase 0 Q5): `var.key <op> operand`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Pushdown {
    /// The property key.
    pub(crate) key: String,
    /// The comparison.
    pub(crate) op: PushOp,
    /// What the property is compared with.
    pub(crate) operand: PushOperand,
}

/// The comparison of a [`Pushdown`] (the `P` / `TextP` the lowering emits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PushOp {
    /// `=`: `P.eq(v)`, never the bare value (type-strict, Phase 0 Q5).
    Eq,
    /// `<`: `P.lt(v)`.
    Lt,
    /// `<=`: `P.lte(v)`.
    Le,
    /// `>`: `P.gt(v)`.
    Gt,
    /// `>=`: `P.gte(v)`.
    Ge,
    /// `IN`: `P.within([..])`.
    In,
    /// `STARTS WITH`: `TextP.starting_with(s)`.
    StartsWith,
    /// `ENDS WITH`: `TextP.ending_with(s)`.
    EndsWith,
    /// `CONTAINS`: `TextP.containing(s)`.
    Contains,
    /// `IS NOT NULL`: exactly `has(key, P.neq(()))`, never `has(key)` (which passes a stored
    /// null).
    NotNull,
}

impl PushOp {
    /// Whether an index can answer it as an equality lookup.
    pub(crate) fn is_equality(self) -> bool {
        matches!(self, PushOp::Eq | PushOp::In)
    }

    /// The order of pushed conjuncts on one element (lower first).
    fn rank(self) -> u8 {
        match self {
            PushOp::Eq => 0,
            PushOp::In => 1,
            PushOp::StartsWith => 2,
            PushOp::Lt | PushOp::Le | PushOp::Gt | PushOp::Ge => 3,
            PushOp::EndsWith | PushOp::Contains => 4,
            PushOp::NotNull => 5,
        }
    }

    /// The comparison with its operands swapped (`1 < x` is `x > 1`).
    fn mirrored(self) -> Option<PushOp> {
        match self {
            PushOp::Eq => Some(PushOp::Eq),
            PushOp::Lt => Some(PushOp::Gt),
            PushOp::Le => Some(PushOp::Ge),
            PushOp::Gt => Some(PushOp::Lt),
            PushOp::Ge => Some(PushOp::Le),
            _ => None,
        }
    }
}

/// The operand of a [`Pushdown`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PushOperand {
    /// `IS NOT NULL` has none.
    None,
    /// A non-null scalar literal (a string for the text forms).
    Value(Literal),
    /// The scalar elements of an `IN` list literal, `NULL` elements removed (equivalent at a
    /// top-level `AND`: a `NULL` element never makes `IN` true).
    List(Vec<Literal>),
    /// `$name`: pushed only when the supplied value fits the form (a non-null scalar; a list of
    /// scalars, `NULL`s removed, for `IN`; a string for the text forms), which the lowering
    /// checks against the bound values; otherwise the conjunct stays in `gql:filter`.
    Parameter {
        /// The parameter name.
        name: String,
        /// The `$name` text.
        span: Span,
    },
}

/// The push-down form of `conjunct`, or `None` when it must stay in `gql:filter`.
///
/// Pushable (Phase 0 Q5): a conjunct over exactly one binding of the form `var.key <op> operand`
/// (or `operand <op> var.key` for `=` and the ranges), `op` one of `=`, `<`, `<=`, `>`, `>=`
/// (operand a non-null scalar literal or a parameter), `IN` (a list literal of scalars and
/// `NULL`s, or a parameter), `STARTS WITH` / `ENDS WITH` / `CONTAINS` (a string literal or a
/// parameter); `var.key IS NOT NULL`; an inline property `{key: operand}` as `=`. Never: `<>`,
/// `IS NULL`, a `NULL` operand, a list or map operand of `=` or a range, a nested list in `IN`,
/// anything under `NOT` / `OR`, an expression operand.
pub(crate) fn classify(conjunct: &Conjunct) -> Option<Pushdown> {
    let [binding] = conjunct.bindings[..] else {
        return None;
    };
    match &conjunct.predicate {
        Predicate::Property {
            binding: b,
            key,
            value,
        } if *b == binding => {
            let operand = scalar_operand(value)?;
            Some(Pushdown {
                key: key.name.clone(),
                op: PushOp::Eq,
                operand,
            })
        }
        Predicate::Property { .. } => None,
        Predicate::Expr(expr) => classify_expr(expr),
    }
}

fn classify_expr(expr: &Expr) -> Option<Pushdown> {
    match &expr.kind {
        ExprKind::IsNull {
            operand,
            negated: true,
        } => Some(Pushdown {
            key: property_key(operand)?.to_owned(),
            op: PushOp::NotNull,
            operand: PushOperand::None,
        }),
        ExprKind::Binary { op, left, right } => {
            let op = match op {
                BinaryOp::Eq => PushOp::Eq,
                BinaryOp::Lt => PushOp::Lt,
                BinaryOp::Le => PushOp::Le,
                BinaryOp::Gt => PushOp::Gt,
                BinaryOp::Ge => PushOp::Ge,
                BinaryOp::In => PushOp::In,
                BinaryOp::StartsWith => PushOp::StartsWith,
                BinaryOp::EndsWith => PushOp::EndsWith,
                BinaryOp::Contains => PushOp::Contains,
                _ => return None,
            };
            if let Some(key) = property_key(left) {
                let operand = match op {
                    PushOp::In => list_operand(right)?,
                    PushOp::StartsWith | PushOp::EndsWith | PushOp::Contains => {
                        string_operand(right)?
                    }
                    _ => scalar_operand(right)?,
                };
                return Some(Pushdown {
                    key: key.to_owned(),
                    op,
                    operand,
                });
            }
            let key = property_key(right)?;
            Some(Pushdown {
                key: key.to_owned(),
                op: op.mirrored()?,
                operand: scalar_operand(left)?,
            })
        }
        _ => None,
    }
}

/// `key` of `var.key`.
fn property_key(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Property { target, key } if matches!(target.kind, ExprKind::Variable(_)) => {
            Some(&key.name)
        }
        _ => None,
    }
}

/// A non-null scalar literal (a signed number too) or a parameter.
fn scalar_operand(expr: &Expr) -> Option<PushOperand> {
    match &expr.kind {
        ExprKind::Parameter(name) => Some(PushOperand::Parameter {
            name: name.clone(),
            span: expr.span,
        }),
        _ => scalar_literal(expr).map(PushOperand::Value),
    }
}

fn scalar_literal(expr: &Expr) -> Option<Literal> {
    match &expr.kind {
        ExprKind::Literal(Literal::Null) => None,
        ExprKind::Literal(literal) => Some(literal.clone()),
        ExprKind::Unary { op, operand } => {
            let ExprKind::Literal(literal) = &operand.kind else {
                return None;
            };
            match (op, literal) {
                (super::ast::UnaryOp::Plus, Literal::Integer(_) | Literal::Float(_)) => {
                    Some(literal.clone())
                }
                (super::ast::UnaryOp::Minus, Literal::Integer(i)) => {
                    i.checked_neg().map(Literal::Integer)
                }
                (super::ast::UnaryOp::Minus, Literal::Float(f)) => Some(Literal::Float(-f)),
                _ => None,
            }
        }
        _ => None,
    }
}

fn string_operand(expr: &Expr) -> Option<PushOperand> {
    match &expr.kind {
        ExprKind::Literal(Literal::String(s)) => {
            Some(PushOperand::Value(Literal::String(s.clone())))
        }
        ExprKind::Parameter(name) => Some(PushOperand::Parameter {
            name: name.clone(),
            span: expr.span,
        }),
        _ => None,
    }
}

fn list_operand(expr: &Expr) -> Option<PushOperand> {
    match &expr.kind {
        ExprKind::List(items) => {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                match &item.kind {
                    ExprKind::Literal(Literal::Null) => {}
                    _ => values.push(scalar_literal(item)?),
                }
            }
            Some(PushOperand::List(values))
        }
        ExprKind::Parameter(name) => Some(PushOperand::Parameter {
            name: name.clone(),
            span: expr.span,
        }),
        _ => None,
    }
}

/// Splits the conjuncts `ids` of one element into pushed (most selective first, `indexed` keys
/// before the rest) and residual ones.
pub(crate) fn element_filters(
    ids: &[usize],
    pushdown: &[Option<Pushdown>],
    indexed: impl Fn(&str) -> bool,
) -> ElementFilters {
    let mut pushed: Vec<(u8, u8, usize)> = Vec::new();
    let mut residual = Vec::new();
    for &id in ids {
        match pushdown.get(id).and_then(Option::as_ref) {
            Some(p) => {
                let index_rank = if indexed(&p.key) && p.op != PushOp::NotNull {
                    u8::from(!p.op.is_equality())
                } else {
                    2
                };
                pushed.push((index_rank, p.op.rank(), id));
            }
            None => residual.push(id),
        }
    }
    pushed.sort_unstable();
    residual.sort_unstable();
    ElementFilters {
        pushed: pushed.into_iter().map(|(_, _, id)| id).collect(),
        residual,
    }
}

// ----- label expressions -----

/// The plain labels of a disjunction `A | B | ..` (a single label too); `None` for any other
/// form.
pub(crate) fn label_disjuncts(expr: &LabelExpr) -> Option<Vec<String>> {
    match &expr.kind {
        LabelExprKind::Label(id) => Some(vec![id.name.clone()]),
        LabelExprKind::Or(left, right) => {
            let mut labels = label_disjuncts(left)?;
            labels.extend(label_disjuncts(right)?);
            Some(labels)
        }
        LabelExprKind::Wildcard | LabelExprKind::Not(_) | LabelExprKind::And(..) => None,
    }
}

/// The `&` factors of every expression in `exprs`, in text order.
pub(crate) fn and_factors(exprs: &[LabelExpr]) -> Vec<&LabelExpr> {
    fn walk<'e>(expr: &'e LabelExpr, out: &mut Vec<&'e LabelExpr>) {
        match &expr.kind {
            LabelExprKind::And(left, right) => {
                walk(left, out);
                walk(right, out);
            }
            _ => out.push(expr),
        }
    }
    let mut out = Vec::new();
    for expr in exprs {
        walk(expr, &mut out);
    }
    out
}

/// The scan labels of a binding (see [`Scan::scan_labels`]).
pub(crate) fn scan_labels(exprs: &[LabelExpr]) -> Option<Vec<String>> {
    and_factors(exprs).into_iter().find_map(label_disjuncts)
}

/// The labels an edge with these label expressions may carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EdgeLabels {
    /// Not known statically (no label expression, a wildcard, a negation).
    Any,
    /// One of these (empty: the expression can never hold for an edge, which has one label).
    Only(BTreeSet<String>),
}

impl EdgeLabels {
    /// The labels an edge satisfying every expression of `exprs` may carry.
    pub(crate) fn of(exprs: &[LabelExpr]) -> EdgeLabels {
        exprs
            .iter()
            .fold(EdgeLabels::Any, |acc, expr| acc.and(Self::of_expr(expr)))
    }

    fn of_expr(expr: &LabelExpr) -> EdgeLabels {
        match &expr.kind {
            LabelExprKind::Label(id) => EdgeLabels::Only(BTreeSet::from([id.name.clone()])),
            LabelExprKind::Wildcard | LabelExprKind::Not(_) => EdgeLabels::Any,
            LabelExprKind::And(left, right) => Self::of_expr(left).and(Self::of_expr(right)),
            LabelExprKind::Or(left, right) => match (Self::of_expr(left), Self::of_expr(right)) {
                (EdgeLabels::Only(mut a), EdgeLabels::Only(b)) => {
                    a.extend(b);
                    EdgeLabels::Only(a)
                }
                _ => EdgeLabels::Any,
            },
        }
    }

    fn and(self, other: EdgeLabels) -> EdgeLabels {
        match (self, other) {
            (EdgeLabels::Any, x) | (x, EdgeLabels::Any) => x,
            (EdgeLabels::Only(a), EdgeLabels::Only(b)) => {
                EdgeLabels::Only(a.intersection(&b).cloned().collect())
            }
        }
    }

    /// Whether one edge could satisfy both (never when either can hold for no edge).
    pub(crate) fn may_overlap(&self, other: &EdgeLabels) -> bool {
        match (self, other) {
            (EdgeLabels::Only(none), _) | (_, EdgeLabels::Only(none)) if none.is_empty() => false,
            (EdgeLabels::Any, _) | (_, EdgeLabels::Any) => true,
            (EdgeLabels::Only(a), EdgeLabels::Only(b)) => !a.is_disjoint(b),
        }
    }
}

/// The push-down forms of every conjunct of `bound`, by conjunct index.
pub(crate) fn classify_all(bound: &BoundQuery) -> Vec<Option<Pushdown>> {
    bound.conjuncts.iter().map(classify).collect()
}
