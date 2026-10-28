//! Calls of saved queries (stage 3h, design D1-D12 of 3h.P0): `CALL folder.name(args) [YIELD
//! ..]`, resolved between parsing and binding.
//!
//! The resolution walks the composite query (`UNION` operands, `NEXT` blocks and `CALL { }`
//! bodies included; a call inside a subquery expression is not reached and the binder refuses
//! it) and replaces every [`Stage::Procedure`] by a [`Stage::Call`] whose
//! [`CallClause::procedure`] names the resolved saved query and whose body is synthesized:
//! `RETURN NULL AS <name>, ..` with one column per yielded name (`FINISH` for a call without
//! columns). The body only gives the binder and the planner the shape of the call's rows: the
//! binder checks the yielded names against the scope like a `CALL { }`'s returned columns, the
//! planner treats it as an uncorrelated `CALL () { }`, and the lowering emits
//! `gql:call_query` for it instead of the body.
//!
//! - **Name (D1, D10).** `CALL a.b.c(..)` calls the saved query `c` (saved-query names are
//!   unique in the catalog) whose folder is exactly `a/b`; `CALL c(..)` one of the root folder.
//!   Not found, or found in another folder: 42002 naming where it is.
//! - **Language (D8).** Only a body written in GQL (dialect [`GQL_QUERY_DIALECT`] or
//!   [`GQL_WRITING_QUERY_DIALECT`]); a Rhai body is 0A000 naming `g.query(..)`.
//! - **Arguments (D2-D5).** Positional, in the order the saved query first reads its
//!   parameters (D9 of the user answers; then any declared parameter it never reads, in
//!   declaration order); constants or the caller's `$parameters`, evaluated here, so arity,
//!   types (the parameter's schema, with its write coercion) and missing values are errors of
//!   the compiled query, before it runs: 42002 for the arity and a missing value, 22G03 for a
//!   value of the wrong type. An aggregate in an argument is 42002, any other read of the row
//!   0A000 (not yet). A trailing argument left out takes the parameter's default. Without
//!   parentheses a standalone call takes each argument from the caller's parameter of the same
//!   name (TCK Call2 [3]); an in-query call only when the query has no parameter.
//! - **YIELD (D6).** A standalone call (the whole query) returns every yielded column, all of
//!   them without `YIELD` or with `YIELD *`; an in-query call names the columns it reads
//!   (required when the query returns any), each a column of the saved query's `RETURN`, each
//!   name once; `YIELD *` only standalone. A call of a query without `RETURN` (one ending in
//!   `FINISH` or a write, VOID) yields nothing and keeps each incoming row once.
//! - **Callee (D7).** The saved query is parsed, resolved (its own calls), bound and planned as
//!   a query of its own with the argument values as its parameters; the lowering lowers it from
//!   one fresh row ([`ResolvedCall::callee`]). Its errors keep their own text and caret.
//! - **Recursion (D11).** A chain of calls is expanded here, so it is bounded here: at most
//!   [`MAX_CALL_DEPTH`] levels, and a saved query already on the chain is 42002.
//! - **Writes (D10, D13).** A callee that writes makes the call a write of the caller (the
//!   binder materializes before it like before a writing `CALL { }`); a callee saved as a query
//!   that only reads ([`GQL_QUERY_DIALECT`]) whose plan writes is refused.
//!
//! Without parameter values ([`resolve`] with `None`, for the analysis of a saved query's body:
//! its parameters and effects) the arguments are only checked for reads of the row, and the
//! callee is bound but not planned ([`ResolvedCall::callee`] `None`).

use super::ast::{
    CallClause, CallScope, CompositeQuery, Expr, ExprKind, Identifier, LinearQuery, Literal,
    ProcedureCall, ProcedureRef, Query, QueryBlock, QueryEnd, ReturnClause, ReturnItem, Stage,
};
use super::bind::{BoundQuery, bind, bind_with_parameters, for_each_child, is_aggregate, suggest};
use super::eval::{Env, Scope, Value, compile, status};
use super::lexer::Span;
use super::parser::parse;
use super::plan::LogicalPlan;
use super::planner::{PlannerFacts, plan};
use crate::error::{
    GqlErrorBuilder, GqlErrorKind, GqlHelpTopic, SAVED_QUERY_READ_ONLY, TraverserError,
};
use crate::graph::GraphStorage;
use crate::graph::definition::{
    DefinitionKind, GQL_QUERY_DIALECT, GQL_WRITING_QUERY_DIALECT, QueryDefinition, QueryParam,
};
use crate::graph::schema::JsonType;
use crate::{CompactString, ElementProperty};

/// The deepest chain of saved queries calling saved queries (D11): the bound of `g.query(..)`
/// (`script::saved_query::MAX_QUERY_DEPTH`), mirrored here because the GQL support does not
/// need the script layer.
pub(crate) const MAX_CALL_DEPTH: usize = 16;

/// The most calls one query resolves, over every level of saved queries calling saved queries
/// (each callee is resolved twice, once to learn its shape and once with its argument values):
/// a query whose saved queries call each other several times per level would otherwise
/// compile exponentially many callees. Far beyond any written call graph; it keeps a query
/// fast to prepare (kind `Limit`, 54001).
pub(crate) const MAX_RESOLVED_CALLS: usize = 1024;

/// Where the resolution looks saved queries up: the catalog of the graph the query runs on.
pub(crate) trait Procedures {
    /// The saved query `name` (`None`: the catalog has none of that name).
    fn saved_query(&self, name: &str) -> Option<QueryDefinition>;
}

/// No catalog: every call is "not found".
#[cfg_attr(
    not(any(test, feature = "script")),
    allow(dead_code, reason = "used by the tests and the script layer")
)]
pub(crate) struct NoProcedures;

impl Procedures for NoProcedures {
    fn saved_query(&self, _name: &str) -> Option<QueryDefinition> {
        None
    }
}

/// The catalog of a storage ([`GraphStorage::definition`]). A catalog read that fails is kept
/// (the lookup answers "not found" meanwhile) and reported by [`Self::take_error`], which the
/// caller asks after the resolution: the storage's error, never "no such query".
pub(crate) struct StorageProcedures<'s, S: GraphStorage + ?Sized> {
    storage: &'s S,
    error: std::cell::RefCell<Option<crate::error::GraphError>>,
}

impl<'s, S: GraphStorage + ?Sized> StorageProcedures<'s, S> {
    /// The catalog of `storage`.
    pub(crate) fn new(storage: &'s S) -> Self {
        Self {
            storage,
            error: std::cell::RefCell::new(None),
        }
    }

    /// The first failed catalog read, if any.
    pub(crate) fn take_error(&self) -> Option<crate::error::GraphError> {
        self.error.borrow_mut().take()
    }
}

impl<S: GraphStorage + ?Sized> Procedures for StorageProcedures<'_, S> {
    fn saved_query(&self, name: &str) -> Option<QueryDefinition> {
        match self.storage.definition(DefinitionKind::QUERY, name) {
            Ok(found) => found.and_then(|d| d.as_query()).cloned(),
            Err(err) => {
                let mut kept = self.error.borrow_mut();
                if kept.is_none() {
                    *kept = Some(err);
                }
                None
            }
        }
    }
}

/// A saved query a call of the query resolved to (stage 3h); the lowering emits
/// `gql:call_query` for it.
pub(crate) struct ResolvedCall {
    /// The saved query's name (what `Execute` names).
    pub(crate) name: String,
    /// Its folder and name as the profile shows it (`test/my/proc`).
    pub(crate) path: String,
    /// The callee, ready to lower; `None` when resolved without parameter values.
    pub(crate) callee: Option<Box<Callee>>,
    /// Per yielded column, in yield order: its name in the caller and the index of the
    /// callee's column it holds.
    pub(crate) outputs: Vec<(CompactString, usize)>,
    /// The callee returns no record (it ends in `FINISH` or a write): each incoming row
    /// passes on once.
    pub(crate) unit: bool,
    /// The call has no parentheses: a standalone call reading its arguments from the caller's
    /// parameters of the same names. Resolved without the catalog
    /// ([`Resolver::deferred`]) the names it reads are not known.
    #[cfg(feature = "script")]
    pub(crate) implicit_arguments: bool,
}

/// The query a call runs: bound and planned with the argument values as its parameters.
pub(crate) struct Callee {
    /// Its text (the saved query's source).
    pub(crate) text: String,
    /// The bound query.
    pub(crate) bound: BoundQuery,
    /// Its plan.
    pub(crate) plan: LogicalPlan,
    /// The argument values by parameter name (every parameter it reads).
    pub(crate) params: Vec<(CompactString, ElementProperty)>,
    /// Its own calls of saved queries.
    pub(crate) calls: Vec<ResolvedCall>,
}

/// What the resolution reads: the catalog, the planner's facts, the chain of saved queries
/// being resolved.
pub(crate) struct Resolver<'a> {
    /// The catalog.
    pub(crate) procedures: &'a dyn Procedures,
    /// The facts the callees are planned with.
    pub(crate) facts: &'a dyn PlannerFacts,
    /// The arguments are placeholders (the analysis of a saved query's plan on a scratch
    /// graph): an argument that does not fit its parameter's schema is replaced by a value
    /// that does, since only the plan's shape is wanted.
    pub(crate) placeholders: bool,
    /// The calls are not looked up (the analysis of a saved query's body without its
    /// catalog, `script/saved_query_gql.rs`): each yields the names its `YIELD` writes (none
    /// without `YIELD`), its arguments are only checked for aggregates and reads of the row,
    /// and it writes nothing of its own; the saved queries it calls are listed in
    /// [`ResolvedCall::name`], whose effects the script layer adds transitively.
    pub(crate) deferred: bool,
    /// The saved queries whose bodies are being resolved, outermost first.
    pub(crate) chain: Vec<String>,
    /// How many calls the whole resolution resolved so far, every level and pass included
    /// (shared by the resolvers of the callees): bounded by [`MAX_RESOLVED_CALLS`].
    resolved: std::rc::Rc<std::cell::Cell<usize>>,
}

impl<'a> Resolver<'a> {
    /// A resolver at the top of a chain.
    pub(crate) fn new(procedures: &'a dyn Procedures, facts: &'a dyn PlannerFacts) -> Self {
        Resolver {
            procedures,
            facts,
            placeholders: false,
            deferred: false,
            chain: Vec::new(),
            resolved: std::rc::Rc::default(),
        }
    }

    /// The resolver of the body of the saved query `name`.
    fn deeper(&self, name: &str) -> Resolver<'a> {
        let mut chain = self.chain.clone();
        chain.push(name.to_string());
        Resolver {
            procedures: self.procedures,
            facts: self.facts,
            placeholders: self.placeholders,
            deferred: self.deferred,
            chain,
            resolved: std::rc::Rc::clone(&self.resolved),
        }
    }
}

/// Resolves every call of a saved query in `query` (module docs), replacing each by a
/// [`Stage::Call`] with its [`CallClause::procedure`]; `params` are the query's parameter
/// values (`None`: not known, the callees are bound but not planned). Returns the resolved
/// calls in the order the `ProcedureRef::index`es name them.
pub(crate) fn resolve(
    query: &mut Query,
    params: Option<&[(CompactString, ElementProperty)]>,
    resolver: &Resolver<'_>,
) -> Result<Vec<ResolvedCall>, TraverserError> {
    if !has_procedures(&query.body) {
        return Ok(Vec::new());
    }
    let source = query.source.clone();
    let mut pass = Pass {
        source: &source,
        params,
        resolver,
        calls: Vec::new(),
    };
    pass.composite(&mut query.body, true)?;
    Ok(pass.calls)
}

/// Whether `composite` holds a call of a saved query in a stage the resolution reaches
/// (nested `CALL { }` bodies included); walked without recursion.
fn has_procedures(composite: &CompositeQuery) -> bool {
    let mut pending = vec![composite];
    while let Some(c) = pending.pop() {
        for linear in linears(c) {
            for stage in &linear.stages {
                match stage {
                    Stage::Procedure(_) => return true,
                    Stage::Call(call) => pending.push(&call.body),
                    Stage::Match(_)
                    | Stage::With(_)
                    | Stage::Let(_)
                    | Stage::Filter(_)
                    | Stage::For(_)
                    | Stage::Insert(_)
                    | Stage::Set(_)
                    | Stage::Remove(_)
                    | Stage::Delete(_)
                    | Stage::Merge(_) => {}
                }
            }
        }
    }
    false
}

/// Every linear query of `composite` (not those of nested bodies).
fn linears(composite: &CompositeQuery) -> impl Iterator<Item = &LinearQuery> {
    std::iter::once(&composite.first)
        .chain(composite.next.iter().map(|n| &n.block))
        .flat_map(|b| std::iter::once(&b.first).chain(b.unions.iter().map(|u| &u.query)))
}

/// One resolution of one query text.
struct Pass<'p, 'r> {
    source: &'p str,
    params: Option<&'p [(CompactString, ElementProperty)]>,
    resolver: &'p Resolver<'r>,
    calls: Vec<ResolvedCall>,
}

/// The analysis of a callee: its bound query (without parameter values) and what a call
/// needs to know of it.
struct Shape {
    /// Its parameters in positional order (first read first, then those it never reads).
    order: Vec<String>,
    /// Its result columns.
    columns: Vec<String>,
    /// It ends without `RETURN`.
    unit: bool,
    /// Its plan writes.
    writes: bool,
}

impl Pass<'_, '_> {
    fn error(&self, builder: GqlErrorBuilder) -> TraverserError {
        builder.into_error(self.source)
    }

    /// The linear queries of `composite`, then the bodies of their `CALL { }` stages (a thin
    /// frame: the recursion is bounded by the parser's nesting bound). `only`: `composite` is
    /// the whole query and one linear query (where a standalone call may stand).
    fn composite(
        &mut self,
        composite: &mut CompositeQuery,
        top: bool,
    ) -> Result<(), TraverserError> {
        let only = top && composite.next.is_empty() && composite.first.unions.is_empty();
        let blocks = std::iter::once(&mut composite.first)
            .chain(composite.next.iter_mut().map(|n| &mut n.block));
        for block in blocks {
            self.block(block, only)?;
        }
        Ok(())
    }

    fn block(&mut self, block: &mut QueryBlock, only: bool) -> Result<(), TraverserError> {
        self.linear(&mut block.first, only)?;
        for operand in &mut block.unions {
            self.linear(&mut operand.query, false)?;
        }
        Ok(())
    }

    fn linear(&mut self, linear: &mut LinearQuery, only: bool) -> Result<(), TraverserError> {
        for i in 0..linear.stages.len() {
            let replacement = match linear.stages.get_mut(i) {
                Some(Stage::Call(call)) => {
                    self.composite(&mut call.body, false)?;
                    None
                }
                Some(Stage::Procedure(call)) => Some(self.procedure(call, only)?),
                _ => None,
            };
            if let Some((clause, ret)) = replacement {
                if let Some(slot) = linear.stages.get_mut(i) {
                    *slot = Stage::Call(Box::new(clause));
                }
                if let Some(ret) = ret {
                    linear.ret = ret;
                }
            }
        }
        Ok(())
    }

    /// Resolves one call: the [`CallClause`] standing for it and, for a standalone call that
    /// returns columns, the `RETURN` of the whole query.
    #[inline(never)]
    fn procedure(
        &mut self,
        call: &ProcedureCall,
        only: bool,
    ) -> Result<(CallClause, Option<ReturnClause>), TraverserError> {
        if call.standalone && !only {
            return Err(self.error(
                semantic(
                    call.span,
                    format!(
                        "CALL {} without RETURN is a whole query; here it is one part of a \
                         larger one",
                        call.dotted()
                    ),
                )
                .alternative(format!(
                    "name the columns it returns with YIELD and end the query with RETURN, as \
                     in CALL {}(..) YIELD x RETURN x",
                    call.dotted()
                )),
            ));
        }
        let resolved = self.resolver.resolved.get().saturating_add(1);
        self.resolver.resolved.set(resolved);
        if resolved > MAX_RESOLVED_CALLS {
            return Err(self.error(
                GqlErrorBuilder::new(
                    GqlErrorKind::Limit,
                    call.name_span().start..call.name_span().end,
                    format!(
                        "the query and the saved queries it calls hold more than \
                         {MAX_RESOLVED_CALLS} calls of saved queries in all"
                    ),
                )
                .alternative(
                    "call fewer saved queries, or write a part of them in the query itself",
                )
                .topic(GqlHelpTopic::Procedure),
            ));
        }
        if self.resolver.deferred {
            return self.deferred(call);
        }
        let (name, definition) = self.lookup(call)?;
        let path = match definition.folder() {
            Some(folder) => format!("{folder}/{name}"),
            None => name.clone(),
        };
        let shape = self.shape(call, &name, &definition)?;
        let (values, parameters) = self.arguments(call, &name, &definition, &shape)?;
        let outputs = self.outputs(call, &shape)?;
        let callee = match &values {
            Some(values) => Some(Box::new(self.callee(&name, &definition, values)?)),
            None => None,
        };
        let index = self.calls.len();
        let dotted = call.dotted();
        let procedure = ProcedureRef {
            index,
            writes: shape.writes,
            dotted,
            name: call.name_span(),
            parameters,
        };
        let clause = synthesized_clause(call, &outputs, shape.unit, procedure);
        let ret = (call.standalone || call.ends_query)
            .then(|| standalone_return(call, &outputs, shape.unit));
        self.calls.push(ResolvedCall {
            name,
            path,
            callee,
            outputs: outputs
                .iter()
                .map(|(name, column)| (CompactString::new(&name.name), *column))
                .collect(),
            unit: shape.unit,
            #[cfg(feature = "script")]
            implicit_arguments: call.args.is_none(),
        });
        Ok((clause, ret))
    }

    /// A call resolved without its catalog ([`Resolver::deferred`]).
    fn deferred(
        &mut self,
        call: &ProcedureCall,
    ) -> Result<(CallClause, Option<ReturnClause>), TraverserError> {
        let name = call.name.last().map(|n| n.name.clone()).unwrap_or_default();
        let path = call
            .name
            .iter()
            .map(|n| n.name.as_str())
            .collect::<Vec<_>>()
            .join("/");
        let mut parameters = Vec::new();
        for arg in call.args.iter().flatten() {
            collect_parameters(arg, &mut parameters);
            self.constant(call, arg)?;
        }
        let outputs: Vec<(Identifier, usize)> = call
            .yields
            .iter()
            .flat_map(|y| &y.items)
            .enumerate()
            .map(|(i, item)| (item.name().clone(), i))
            .collect();
        let unit = outputs.is_empty();
        let procedure = ProcedureRef {
            index: self.calls.len(),
            writes: false,
            dotted: call.dotted(),
            name: call.name_span(),
            parameters,
        };
        let clause = synthesized_clause(call, &outputs, unit, procedure);
        let ret =
            (call.standalone || call.ends_query).then(|| standalone_return(call, &outputs, unit));
        self.calls.push(ResolvedCall {
            name,
            path,
            callee: None,
            outputs: outputs
                .iter()
                .map(|(name, column)| (CompactString::new(&name.name), *column))
                .collect(),
            unit,
            #[cfg(feature = "script")]
            implicit_arguments: call.args.is_none(),
        });
        Ok((clause, ret))
    }

    /// The saved query a call names (D1, D10, D8, D11): its name and definition.
    fn lookup(&self, call: &ProcedureCall) -> Result<(String, QueryDefinition), TraverserError> {
        let span = call.name_span();
        let Some((last, folder)) = call.name.split_last() else {
            return Err(self.error(semantic(span, "a CALL without a saved query name")));
        };
        let name = last.name.clone();
        let folder: Vec<&str> = folder.iter().map(|s| s.name.as_str()).collect();
        let wanted = (!folder.is_empty()).then(|| folder.join("/"));
        let found = self.resolver.procedures.saved_query(&name);
        let definition = match found {
            Some(definition) if definition.folder() == wanted.as_deref() => definition,
            Some(definition) => {
                let place = match definition.folder() {
                    Some(f) => format!("in the folder `{f}`"),
                    None => "in the root folder".to_string(),
                };
                let here = match &wanted {
                    Some(w) => format!("in the folder `{w}`"),
                    None => "in the root folder".to_string(),
                };
                return Err(self.error(
                    semantic(
                        span,
                        format!(
                            "there is no saved query `{name}` {here}: the saved query \
                             `{name}` is {place}"
                        ),
                    )
                    .suggestion(format!("CALL {}", qualified(definition.folder(), &name)))
                    .topic(GqlHelpTopic::Procedure),
                ));
            }
            None => {
                let here = match &wanted {
                    Some(w) => format!(" in the folder `{w}`"),
                    None => " in the root folder".to_string(),
                };
                return Err(self.error(
                    semantic(span, format!("there is no saved query `{name}`{here}"))
                        .alternative(format!(
                            "save it first with g.define_query(#{{name: {}, {}language: \
                             \"gql\", body: \"..\"}}); g.queries() lists the saved queries",
                            crate::value::literal::rhai_string(&name),
                            wanted
                                .as_ref()
                                .map(|w| format!(
                                    "folder: {}, ",
                                    crate::value::literal::rhai_string(w)
                                ))
                                .unwrap_or_default()
                        ))
                        .topic(GqlHelpTopic::Procedure),
                ));
            }
        };
        if let Some(position) = self.resolver.chain.iter().position(|n| *n == name) {
            let cycle: Vec<&str> = self
                .resolver
                .chain
                .get(position..)
                .unwrap_or_default()
                .iter()
                .map(String::as_str)
                .chain(std::iter::once(name.as_str()))
                .collect();
            return Err(self.error(
                semantic(
                    span,
                    format!(
                        "recursive call of the saved query `{name}`: {} (the arguments of a \
                         CALL are constants, so the recursion would never end)",
                        cycle.join(" calls ")
                    ),
                )
                .topic(GqlHelpTopic::Procedure),
            ));
        }
        if self.resolver.chain.len() >= MAX_CALL_DEPTH {
            return Err(self.error(
                GqlErrorBuilder::new(
                    GqlErrorKind::Limit,
                    span.start..span.end,
                    format!(
                        "saved queries call each other more than {MAX_CALL_DEPTH} levels deep \
                         (CALL {} at the end of {})",
                        call.dotted(),
                        self.resolver.chain.join(" -> ")
                    ),
                )
                .topic(GqlHelpTopic::Procedure),
            ));
        }
        if let Some(reason) = definition.unsupported_reason() {
            return Err(self.error(
                GqlErrorBuilder::new(
                    GqlErrorKind::Refused,
                    span.start..span.end,
                    format!("the saved query `{name}` cannot be called: {reason}"),
                )
                .topic(GqlHelpTopic::Procedure),
            ));
        }
        let dialect = definition.dialect();
        if dialect != GQL_QUERY_DIALECT && dialect != GQL_WRITING_QUERY_DIALECT {
            let builder = if definition.language() == "rhai" {
                GqlErrorBuilder::new(
                    GqlErrorKind::NotYet,
                    span.start..span.end,
                    format!(
                        "the saved query `{name}` is written in Rhai; CALL runs saved queries \
                         written in GQL in this version"
                    ),
                )
                .alternative(format!(
                    "run it from the DSL with g.query({}, #{{..}}), or save it again in GQL \
                     (language: \"gql\")",
                    crate::value::literal::rhai_string(&name)
                ))
            } else {
                GqlErrorBuilder::new(
                    GqlErrorKind::Refused,
                    span.start..span.end,
                    format!(
                        "the saved query `{name}` has the dialect '{dialect}'; CALL runs \
                         {GQL_QUERY_DIALECT} and {GQL_WRITING_QUERY_DIALECT}"
                    ),
                )
            };
            return Err(self.error(builder.topic(GqlHelpTopic::Procedure)));
        }
        Ok((name, definition))
    }

    /// The analysis of the callee: bound without parameter values (its own calls resolved the
    /// same way).
    fn shape(
        &self,
        call: &ProcedureCall,
        name: &str,
        definition: &QueryDefinition,
    ) -> Result<Shape, TraverserError> {
        let mut parsed = parse(definition.source())?;
        resolve(&mut parsed, None, &self.resolver.deeper(name))?;
        let bound = bind(&parsed)?;
        let mut order: Vec<String> = Vec::new();
        for parameter in &bound.parameters {
            if !order.contains(&parameter.name) {
                order.push(parameter.name.clone());
            }
        }
        for (declared, _) in definition.params() {
            if !order.contains(declared) {
                order.push(declared.clone());
            }
        }
        let last = &bound.body.last().first;
        let unit = last.end != QueryEnd::Return;
        let columns = last.columns.iter().map(|c| c.name.clone()).collect();
        let writes = bound.writes();
        if writes && definition.dialect() == GQL_QUERY_DIALECT {
            return Err(self.error(
                GqlErrorBuilder::new(
                    GqlErrorKind::Refused,
                    call.name_span().start..call.name_span().end,
                    format!(
                        "{SAVED_QUERY_READ_ONLY}: `{name}` is saved as a query that only reads \
                         (dialect {GQL_QUERY_DIALECT}), but its query writes"
                    ),
                )
                .alternative("save it again with g.define_query(..), which records that it writes")
                .topic(GqlHelpTopic::Procedure),
            ));
        }
        Ok(Shape {
            order,
            columns,
            unit,
            writes,
        })
    }

    /// The argument values by parameter name (D2-D5), `None` without parameter values, and the
    /// caller's parameters the call reads.
    #[allow(clippy::type_complexity)]
    fn arguments(
        &self,
        call: &ProcedureCall,
        name: &str,
        definition: &QueryDefinition,
        shape: &Shape,
    ) -> Result<
        (
            Option<Vec<(CompactString, ElementProperty)>>,
            Vec<(String, Span)>,
        ),
        TraverserError,
    > {
        let span = call.name_span();
        let signature = || {
            let list: Vec<String> = shape
                .order
                .iter()
                .map(|p| match definition.param(p) {
                    Some(param) => format!("{p} :: {}", type_text(param)),
                    None => p.clone(),
                })
                .collect();
            format!("{}({})", call.dotted(), list.join(", "))
        };
        let param_of = |parameter: &str| {
            definition.param(parameter).ok_or_else(|| {
                self.error(
                    semantic(
                        span,
                        format!(
                            "the saved query `{name}` reads the parameter ${parameter}, which \
                             it does not declare"
                        ),
                    )
                    .topic(GqlHelpTopic::Procedure),
                )
            })
        };
        let mut values = Vec::with_capacity(shape.order.len());
        let mut parameters = Vec::new();
        let Some(args) = &call.args else {
            // Implicit arguments (no parentheses): a standalone call reads each from the
            // caller's parameter of the same name; an in-query call only without parameters.
            if !call.standalone && !shape.order.is_empty() {
                return Err(self.error(
                    semantic(
                        span,
                        format!(
                            "CALL {} inside a query passes its arguments in parentheses; only a \
                             standalone call reads them from the query's parameters by name",
                            call.dotted()
                        ),
                    )
                    .alternative(format!(
                        "write the arguments, as in CALL {}",
                        example_call(call, &shape.order)
                    ))
                    .topic(GqlHelpTopic::Procedure),
                ));
            }
            for parameter in &shape.order {
                let param = param_of(parameter)?;
                let given = self.params.and_then(|params| {
                    params
                        .iter()
                        .find(|(n, _)| n.as_str() == parameter)
                        .map(|(_, v)| v.clone())
                });
                // The call reads the caller's parameter unless its default stands in for a
                // value the caller does not give (without values, it may be given).
                if self.params.is_none() || given.is_some() || param.default_value().is_none() {
                    parameters.push((parameter.clone(), span));
                }
                if self.params.is_none() {
                    continue;
                }
                let value = match (given, param.default_value()) {
                    (Some(value), _) => value,
                    (None, Some(default)) => default.clone(),
                    (None, None) => {
                        return Err(self.error(
                            semantic(
                                span,
                                format!(
                                    "the parameter `${parameter}` has no value: CALL {} without \
                                     parentheses reads its arguments from the query's parameters \
                                     of the same names",
                                    call.dotted()
                                ),
                            )
                            .alternative(format!(
                                "pass the parameter, or write the arguments, as in CALL {}",
                                example_call(call, &shape.order)
                            ))
                            .topic(GqlHelpTopic::Procedure),
                        ));
                    }
                };
                let value = self.checked(call, name, parameter, param, value, span)?;
                values.push((CompactString::new(parameter), value));
            }
            return Ok((self.params.map(|_| values), parameters));
        };
        if args.len() > shape.order.len() {
            let extra = args.get(shape.order.len()).map_or(span, |a| a.span);
            return Err(self.error(
                semantic(
                    extra,
                    format!(
                        "too many arguments: {} takes {}, {} given",
                        signature(),
                        count(shape.order.len()),
                        args.len()
                    ),
                )
                .topic(GqlHelpTopic::Procedure),
            ));
        }
        for (i, parameter) in shape.order.iter().enumerate() {
            let param = param_of(parameter)?;
            let Some(arg) = args.get(i) else {
                match param.default_value() {
                    Some(default) => {
                        values.push((CompactString::new(parameter), default.clone()));
                        continue;
                    }
                    None => {
                        return Err(self.error(
                            semantic(
                                span,
                                format!(
                                    "missing argument for `{parameter}`: {} takes {}, {} given",
                                    signature(),
                                    count(shape.order.len()),
                                    args.len()
                                ),
                            )
                            .topic(GqlHelpTopic::Procedure),
                        ));
                    }
                }
            };
            collect_parameters(arg, &mut parameters);
            let Some(value) = self.constant(call, arg)? else {
                continue;
            };
            let value = self.checked(call, name, parameter, param, value, arg.span)?;
            values.push((CompactString::new(parameter), value));
        }
        Ok((self.params.map(|_| values), parameters))
    }

    /// The value of argument `arg` (`None` without parameter values): an aggregate is 42002,
    /// a read of the row 0A000.
    fn constant(
        &self,
        call: &ProcedureCall,
        arg: &Expr,
    ) -> Result<Option<ElementProperty>, TraverserError> {
        let mut pending = vec![arg];
        while let Some(expr) = pending.pop() {
            if let ExprKind::FunctionCall { name, .. } = &expr.kind
                && is_aggregate(name)
            {
                return Err(self.error(
                    semantic(
                        expr.span,
                        format!(
                            "an argument of CALL {} cannot aggregate: it is evaluated once, \
                             before the call",
                            call.dotted()
                        ),
                    )
                    .alternative(format!(
                        "aggregate in a WITH before the query, or call the saved query with \
                         a constant, as in CALL {}",
                        example_call(call, &[])
                    ))
                    .topic(GqlHelpTopic::Procedure),
                ));
            }
            for_each_child(expr, &mut |child| pending.push(child));
        }
        let scope = ArgScope {
            params: self.params,
        };
        let compiled = match compile::<()>(arg, &scope) {
            Ok(compiled) => compiled,
            Err(error) if error.gqlstatus == status::SEMANTIC && self.params.is_some() => {
                // A parameter without a value, or a variable: a read of the row.
                let missing = error.message.starts_with("no value was given");
                let builder = if missing {
                    semantic(error.span, error.message)
                } else {
                    GqlErrorBuilder::new(
                        GqlErrorKind::NotYet,
                        arg.span.start..arg.span.end,
                        format!(
                            "an argument of CALL {} that reads the row is not yet supported \
                             ({})",
                            call.dotted(),
                            error.message
                        ),
                    )
                    .alternative(
                        "pass a constant or a $parameter, or write the saved query's pattern \
                         in the query itself",
                    )
                };
                return Err(self.error(builder.topic(GqlHelpTopic::Procedure)));
            }
            Err(error) if error.gqlstatus == status::SEMANTIC => {
                return Err(self.error(
                    GqlErrorBuilder::new(
                        GqlErrorKind::NotYet,
                        arg.span.start..arg.span.end,
                        format!(
                            "an argument of CALL {} that reads the row is not yet supported ({})",
                            call.dotted(),
                            error.message
                        ),
                    )
                    .alternative("pass a constant or a $parameter")
                    .topic(GqlHelpTopic::Procedure),
                ));
            }
            Err(error) => return Err(super::runtime::eval_error(self.source, error)),
        };
        // Without parameter values an argument reading a parameter is not known yet; any
        // other is (and is checked against its parameter when the query is saved).
        let mut reads = Vec::new();
        collect_parameters(arg, &mut reads);
        if self.params.is_none() && !reads.is_empty() {
            return Ok(None);
        }
        let value = compiled
            .eval(&NoRow)
            .map_err(|error| super::runtime::eval_error(self.source, error))?;
        Ok(Some(value.into_property(&mut |()| ElementProperty::Null)))
    }

    /// `value` checked against `param` (its schema's write coercion applied): 22G03 at `span`
    /// when it does not fit (a placeholder is replaced by one that fits).
    fn checked(
        &self,
        call: &ProcedureCall,
        name: &str,
        parameter: &str,
        param: &QueryParam,
        value: ElementProperty,
        span: Span,
    ) -> Result<ElementProperty, TraverserError> {
        match param.check_value(name, parameter, value) {
            Ok(value) => Ok(value),
            Err(_) if self.resolver.placeholders => Ok(placeholder(name, parameter, param)),
            Err(error) => Err(self.error(
                GqlErrorBuilder::new(
                    GqlErrorKind::Data,
                    span.start..span.end,
                    format!(
                        "the argument for `{parameter}` of CALL {} must be {}: {error}",
                        call.dotted(),
                        type_text(param)
                    ),
                )
                .gqlstatus(status::INVALID_VALUE_TYPE)
                .topic(GqlHelpTopic::Procedure),
            )),
        }
    }

    /// The yielded columns (D6): per name in the caller, the callee's column it holds.
    fn outputs(
        &self,
        call: &ProcedureCall,
        shape: &Shape,
    ) -> Result<Vec<(Identifier, usize)>, TraverserError> {
        let span = call.name_span();
        let all = || {
            shape
                .columns
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    (
                        Identifier {
                            name: c.clone(),
                            span,
                        },
                        i,
                    )
                })
                .collect::<Vec<_>>()
        };
        let Some(yields) = &call.yields else {
            if call.standalone || shape.unit {
                return Ok(all());
            }
            return Err(self.error(
                semantic(
                    span,
                    format!(
                        "CALL {} inside a query names the columns it reads with YIELD: the \
                         saved query returns {}",
                        call.dotted(),
                        list(&shape.columns)
                    ),
                )
                .alternative(if call.ends_query {
                    // At the end of the query: only a VOID call ends one without RETURN.
                    format!(
                        "add YIELD and end the query with RETURN, as in CALL {}(..) YIELD {cols} \
                         RETURN {cols}",
                        call.dotted(),
                        cols = shape.columns.join(", ")
                    )
                } else {
                    format!(
                        "add YIELD, as in CALL {}(..) YIELD {}",
                        call.dotted(),
                        shape.columns.join(", ")
                    )
                })
                .topic(GqlHelpTopic::Procedure),
            ));
        };
        if let Some(star) = yields.star {
            if call.standalone {
                return Ok(all());
            }
            return Err(self.error(
                GqlErrorBuilder::new(
                    GqlErrorKind::Syntax,
                    star.start..star.end,
                    "YIELD * is allowed only in a standalone CALL (the whole query)",
                )
                .alternative(format!(
                    "name the columns, as in CALL {}(..) YIELD {}",
                    call.dotted(),
                    shape.columns.join(", ")
                ))
                .topic(GqlHelpTopic::Procedure),
            ));
        }
        if shape.unit {
            return Err(self.error(
                semantic(
                    yields.span,
                    format!(
                        "the saved query called by CALL {} returns no columns (it ends without \
                         RETURN), so there is nothing to YIELD",
                        call.dotted()
                    ),
                )
                .alternative("remove YIELD")
                .topic(GqlHelpTopic::Procedure),
            ));
        }
        let mut outputs: Vec<(Identifier, usize)> = Vec::with_capacity(yields.items.len());
        for item in &yields.items {
            let Some(column) = shape.columns.iter().position(|c| *c == item.column.name) else {
                let mut builder = semantic(
                    item.column.span,
                    format!(
                        "`{}` is not a column of the saved query called by CALL {}; it returns \
                         {}",
                        item.column.name,
                        call.dotted(),
                        list(&shape.columns)
                    ),
                )
                .topic(GqlHelpTopic::Procedure);
                if let Some(close) =
                    suggest(&item.column.name, shape.columns.iter().map(String::as_str))
                {
                    builder = builder.suggestion(format!("`{close}`"));
                }
                return Err(self.error(builder));
            };
            let target = item.name();
            if outputs.iter().any(|(name, _)| name.name == target.name) {
                return Err(self.error(
                    semantic(
                        target.span,
                        format!(
                            "`{}` is yielded twice by CALL {}; every yielded name must be new",
                            target.name,
                            call.dotted()
                        ),
                    )
                    .alternative(format!(
                        "rename one with AS, as in YIELD {} AS {}2",
                        item.column.name, target.name
                    ))
                    .topic(GqlHelpTopic::Procedure),
                ));
            }
            outputs.push((target.clone(), column));
        }
        Ok(outputs)
    }

    /// The callee bound and planned with the argument `values` (D7).
    fn callee(
        &self,
        name: &str,
        definition: &QueryDefinition,
        values: &[(CompactString, ElementProperty)],
    ) -> Result<Callee, TraverserError> {
        let text = definition.source().to_string();
        let mut parsed = parse(&text)?;
        let calls = resolve(&mut parsed, Some(values), &self.resolver.deeper(name))?;
        let bound = bind_with_parameters(&parsed, values.iter().map(|(n, _)| n.as_str()))?;
        let plan = plan(&bound, self.resolver.facts, None);
        Ok(Callee {
            text,
            bound,
            plan,
            params: values.to_vec(),
            calls,
        })
    }
}

/// The [`CallClause`] standing for a resolved call (module docs): an uncorrelated `CALL () {
/// RETURN NULL AS <name>, .. }`, or `CALL () { FINISH }` for a callee without columns.
fn synthesized_clause(
    call: &ProcedureCall,
    outputs: &[(Identifier, usize)],
    unit: bool,
    procedure: ProcedureRef,
) -> CallClause {
    let span = call.span;
    let items = if unit {
        Vec::new()
    } else {
        outputs
            .iter()
            .map(|(name, _)| ReturnItem {
                expr: Expr {
                    kind: ExprKind::Literal(Literal::Null),
                    span: name.span,
                },
                alias: Some(name.clone()),
                span: name.span,
            })
            .collect()
    };
    let linear = LinearQuery {
        matches: Vec::new(),
        stages: Vec::new(),
        // A non-empty span without items ends the body like `FINISH` (`LinearQuery::end`).
        ret: ReturnClause {
            distinct: false,
            star: None,
            items,
            group_by: None,
            span,
        },
        order_by: None,
        offset: None,
        limit: None,
        importing: None,
        span,
    };
    CallClause {
        optional: call.optional,
        scope: CallScope::Variables {
            variables: Vec::new(),
            span: call.keyword,
        },
        body: Box::new(CompositeQuery {
            first: QueryBlock {
                first: linear,
                unions: Vec::new(),
                span,
            },
            next: Vec::new(),
            span,
        }),
        span,
        procedure: Some(procedure),
    }
}

/// The `RETURN` of a standalone call: every yielded name, in yield order; for a saved query
/// without columns (`unit`) an empty one spanning the call, which ends the query like `FINISH`.
fn standalone_return(
    call: &ProcedureCall,
    outputs: &[(Identifier, usize)],
    unit: bool,
) -> ReturnClause {
    if unit {
        return ReturnClause {
            distinct: false,
            star: None,
            items: Vec::new(),
            group_by: None,
            span: call.span,
        };
    }
    ReturnClause {
        distinct: false,
        star: None,
        items: outputs
            .iter()
            .map(|(name, _)| ReturnItem {
                expr: Expr {
                    kind: ExprKind::Variable(name.clone()),
                    span: name.span,
                },
                alias: Some(name.clone()),
                span: name.span,
            })
            .collect(),
        group_by: None,
        span: call.span,
    }
}

/// Every `$name` `expr` reads, with its text.
fn collect_parameters(expr: &Expr, out: &mut Vec<(String, Span)>) {
    let mut pending = vec![expr];
    while let Some(expr) = pending.pop() {
        if let ExprKind::Parameter(name) = &expr.kind {
            out.push((name.clone(), expr.span));
        }
        for_each_child(expr, &mut |child| pending.push(child));
    }
}

/// `folder.name` as a `CALL` writes it (segments that are no plain identifier back-quoted).
fn qualified(folder: Option<&str>, name: &str) -> String {
    folder
        .into_iter()
        .flat_map(|f| f.split('/'))
        .chain(std::iter::once(name))
        .map(quoted)
        .collect::<Vec<_>>()
        .join(".")
}

/// A name segment as written in GQL: bare when it is a plain identifier, else back-quoted.
fn quoted(segment: &str) -> String {
    let plain = segment.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain {
        segment.to_string()
    } else {
        format!("`{}`", segment.replace('`', "``"))
    }
}

/// `name(arg, ..)` with one placeholder per parameter, for advice.
fn example_call(call: &ProcedureCall, order: &[String]) -> String {
    let args: Vec<String> = order.iter().map(|p| format!("${p}")).collect();
    format!("{}({})", call.dotted(), args.join(", "))
}

/// `n argument(s)`.
fn count(n: usize) -> String {
    match n {
        1 => "1 argument".to_string(),
        n => format!("{n} arguments"),
    }
}

/// `` `a`, `b` `` (or "no column").
fn list(columns: &[String]) -> String {
    if columns.is_empty() {
        return "no column".to_string();
    }
    columns
        .iter()
        .map(|c| format!("`{c}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The type of a parameter as its schema names it, in the JSON Schema spelling
/// (`integer`, `string or null`; `uuid` for a string of that format).
fn type_text(param: &QueryParam) -> String {
    let schema = param.schema();
    let Some(types) = &schema.types else {
        return "any value".to_string();
    };
    let uuid = matches!(
        schema.format,
        Some(crate::graph::schema::SchemaFormat::Uuid)
    );
    let mut names: Vec<&str> = types
        .iter()
        .filter(|t| **t != JsonType::Null)
        .map(|t| match t {
            JsonType::String if uuid => "uuid",
            other => other.as_str(),
        })
        .collect();
    if types.contains(&JsonType::Null) {
        names.push("null");
    }
    names.join(" or ")
}

/// A value `param` accepts, for a placeholder argument ([`Resolver::placeholders`]).
fn placeholder(name: &str, parameter: &str, param: &QueryParam) -> ElementProperty {
    if let Some(default) = param.default_value() {
        return default.clone();
    }
    let candidates = [
        ElementProperty::Null,
        ElementProperty::from(0_i64),
        ElementProperty::from(0.0_f64),
        ElementProperty::from(""),
        ElementProperty::from("00000000-0000-0000-0000-000000000000"),
        ElementProperty::from(false),
        ElementProperty::Array(Vec::new()),
        ElementProperty::Object(Box::default()),
    ];
    candidates
        .into_iter()
        .find_map(|candidate| param.check_value(name, parameter, candidate).ok())
        .unwrap_or(ElementProperty::Null)
}

fn semantic(span: Span, message: impl Into<String>) -> GqlErrorBuilder {
    GqlErrorBuilder::new(GqlErrorKind::Semantic, span.start..span.end, message)
}

/// What an argument resolves names against: no variable, the caller's parameters (any
/// parameter is `NULL` without values: only the compile matters then).
struct ArgScope<'a> {
    params: Option<&'a [(CompactString, ElementProperty)]>,
}

impl Scope for ArgScope<'_> {
    fn variable(&self, _: &str) -> Option<usize> {
        None
    }

    fn parameter(&self, name: &str) -> Option<ElementProperty> {
        match self.params {
            Some(params) => params
                .iter()
                .find(|(n, _)| n.as_str() == name)
                .map(|(_, v)| v.clone()),
            None => Some(ElementProperty::Null),
        }
    }
}

/// The row of a constant argument: no variable, no element.
struct NoRow;

impl Env for NoRow {
    type Element = ();

    fn variable(&self, _: usize) -> Value<()> {
        Value::Null
    }

    fn kind(&self, _: &()) -> crate::ElementKind {
        crate::ElementKind::Vertex
    }

    fn element_id(&self, _: &()) -> CompactString {
        CompactString::default()
    }

    fn property(&self, _: &(), _: &str) -> Option<ElementProperty> {
        None
    }

    fn has_label(&self, _: &(), _: &str) -> bool {
        false
    }

    fn has_any_label(&self, _: &()) -> bool {
        false
    }

    fn labels(&self, _: &()) -> Vec<CompactString> {
        Vec::new()
    }
}

#[cfg(test)]
#[path = "procedure/tests.rs"]
mod tests;
