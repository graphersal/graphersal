//! The effects of a saved query (`book/src/users_guide/saved_queries.md`, "Queries that
//! write"): the data writes its body may make, inferred from the Rhai AST of the body and never
//! declared or stored.
//!
//! The inference walks every expression of the body. Each method chain (`g.e().has_label("x")
//! .drop()`) is folded step by step with the rules the authorization check applies to a plan
//! (`GraphStep::access_flow_after` / `GraphStep::for_each_access_in`): a write names the element
//! kind and labels the chain states before it, exactly as the request the engine makes at run
//! time does, so the run-time check can hold every request against the inferred set. Whatever
//! the chain does not state is left open (`Data { None, None }` covers every element), which
//! keeps the inference sound: it may claim more than a body writes, never less.
//!
//! A `g.gql(query)` call with a literal query is lowered as the engine lowers it (on an empty
//! scratch graph, [`crate::gql::lower::planned_writes`]) and its plan folded with the same
//! rules, so a GQL write names the labels it creates exactly like its Gremlin twin. A query
//! text the AST does not give (a variable, a concatenation), or one whose plan needs a
//! parameter's value (`LIMIT $n`), may write anything: the body claims every data write, so it
//! is saved as a writing query and a caller must be allowed to write. Pass values as
//! parameters (`g.gql("... $name ...", #{name: name})`) to keep a read query read-only.
//!
//! | Body | Inferred |
//! |---|---|
//! | `add_v("person")`, `add_e("knows")` | `Create` on that label; a label from a variable: any label of the kind |
//! | `property(..)`, `drop()`, `remove_property(..)` after a chain stating kind / label | `Update` / `Delete` on them |
//! | the same where the chain states nothing (a variable, `__`) | `Update` / `Delete` on any data |
//! | `merge_v` / `merge_e` | `Create` and `Update` on the kind |
//! | a same-named method on something else (`map.drop()`) | counted as a write |
//! | `g.query("name")` | the effects of that query (resolved when needed, [`transitive`]) |
//! | `g.gql("INSERT ...")` (feature `gql`) | the writes of the lowered query's plan (a literal query text) |
//! | `g.gql(text)` with a computed text | `Create`, `Update` and `Delete` on any data |
//!
//! Constructs that hide what a body calls (`Fn(..)`, `call`, `curry`, `eval`, `import`, a
//! function pointer to a named function, `g.query(name)` with a computed name) make the body
//! *dynamic*: such a query stays read-only, and a dynamic body that also writes is refused when
//! it is defined. Writes of the schema, the catalog or files (`set_schema`, `define_query`,
//! `create_index`, `export_graphml`, ...) are refused: a saved query writes data only.

use std::collections::HashSet;

use rhai::{ASTNode, Expr, Stmt};

use crate::graph::{AccessRequest, AccessRequestBuf, Action, ElementKind, Resource};
use crate::traversal::step::AccessFlow;

/// What the body of one saved query itself may do (its calls of other queries not resolved).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BodyEffects {
    /// The data writes of the body's own steps, each once, in the order of their text.
    pub(crate) writes: Vec<AccessRequestBuf>,
    /// The saved queries the body calls by a literal name, each once.
    pub(crate) calls: Vec<String>,
    /// The first construct that hides what the body calls (`"Fn"`, `"call"`, ...), if any.
    pub(crate) dynamic: Option<String>,
}

/// How a DSL function acts on a method chain: what it writes and what it yields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepClass {
    /// Yields vertices (`v()`, `out()`, the endpoint steps).
    Vertices,
    /// Yields edges, labelled by its string arguments (`out_e("knows")`).
    EdgesLabelled,
    /// Yields edges whose labels the arguments do not give (`e(ids)`).
    Edges,
    /// `add_v(labels)`: creates vertices with the literal labels.
    AddV,
    /// `add_e(label)`: creates edges with the literal label.
    AddE,
    /// `merge_v()` / `merge_e()`: creates or updates elements of the kind.
    Merge(ElementKind),
    /// Updates the received elements (`property`, `remove_property`).
    Update,
    /// Updates the received elements of the kind and forgets their labels (`add_label`,
    /// `drop_label` on vertices, `set_label` on edges).
    Relabel(ElementKind),
    /// `drop()`: deletes the received elements.
    Drop,
    /// `has_label(labels)`: keeps the kind, states the labels of edges.
    HasLabel,
    /// Passes its input on unchanged (filters, side effects, modulators).
    Keep,
    /// Passes its input on unchanged when its arguments are plain numbers or strings
    /// (`limit(2)`; `limit(Scope.local, 2)` is a map step).
    KeepPlain,
    /// `g.query(name, ..)`: calls a saved query.
    Query,
    /// `g.gql(query, ..)`: the writes of the lowered GQL query (feature `gql`).
    #[cfg(feature = "gql")]
    Gql,
    /// `with_side_effect(name, graph)`: a `subgraph()` into another graph creates data.
    SideEffectTarget,
    /// Hides what is called (`Fn`, `call`, `curry`, `eval`).
    Dynamic,
    /// Writes the schema, the catalog, files or the journal: never in a saved query.
    Refused(&'static str),
}

const SCHEMA: &str = "it changes the schema";
const CATALOG: &str = "it changes the catalog (saved queries, indexes, compression rules)";
const FILES: &str = "it reads or writes a file";

/// The class of every DSL function that writes, hides calls or keeps a chain's flow, by name in
/// both spellings. Every other name yields a value the chain does not state and writes nothing.
/// `effects_tests.rs` checks every listed name is registered and, by running every documented
/// example, that no unlisted name writes.
pub(crate) const CLASSES: &[(&[&str], StepClass)] = {
    use StepClass::*;
    &[
        (
            &[
                "v", "V", "out", "in", "in_", "both", "out_v", "outV", "in_v", "inV", "both_v",
                "bothV", "other_v", "otherV",
            ],
            Vertices,
        ),
        (
            &["out_e", "outE", "in_e", "inE", "both_e", "bothE"],
            EdgesLabelled,
        ),
        (&["e", "E"], Edges),
        (&["add_v", "addV"], AddV),
        (&["add_e", "addE"], AddE),
        (&["merge_v", "mergeV"], Merge(ElementKind::Vertex)),
        (&["merge_e", "mergeE"], Merge(ElementKind::Edge)),
        (
            &[
                "property",
                "property_json",
                "propertyJson",
                "remove_property",
                "removeProperty",
            ],
            Update,
        ),
        (
            &["add_label", "addLabel", "drop_label", "dropLabel"],
            Relabel(ElementKind::Vertex),
        ),
        (&["set_label", "setLabel"], Relabel(ElementKind::Edge)),
        (&["drop"], Drop),
        (&["has_label", "hasLabel"], HasLabel),
        (
            &[
                "has",
                "has_not",
                "hasNot",
                "has_id",
                "hasId",
                "has_key",
                "hasKey",
                "has_value",
                "hasValue",
                "has_p",
                "is",
                "is_",
                "where",
                "where_p",
                "where_t",
                "filter",
                "not",
                "and",
                "or",
                "simple_path",
                "simplePath",
                "cyclic_path",
                "cyclicPath",
                "as",
                "as_",
                "by",
                "from",
                "to",
                "identity",
                "barrier",
                "aggregate",
                "store",
                "side_effect",
                "sideEffect",
            ],
            Keep,
        ),
        (
            &["dedup", "limit", "order", "range", "sample", "skip", "tail"],
            KeepPlain,
        ),
        (&["query"], Query),
        (&["with_side_effect", "withSideEffect"], SideEffectTarget),
        (&["Fn", "call", "curry", "eval"], Dynamic),
        (
            &[
                "set_schema",
                "setSchema",
                "patch_schema",
                "patchSchema",
                "embed_property",
                "embedProperty",
            ],
            Refused(SCHEMA),
        ),
        (
            &[
                "define_query",
                "defineQuery",
                "define_queries",
                "defineQueries",
                "drop_query",
                "dropQuery",
                "move_query",
                "moveQuery",
                "describe_query",
                "describeQuery",
                "create_index",
                "createIndex",
                "drop_index",
                "dropIndex",
                "define_compression",
                "defineCompression",
                "drop_compression",
                "dropCompression",
                "recompress",
                "define_job",
                "drop_job",
            ],
            Refused(CATALOG),
        ),
        (
            &[
                "import_graphml",
                "importGraphml",
                "import_graphson",
                "importGraphson",
                "export_graphml",
                "exportGraphml",
                "export_graphson",
                "exportGraphson",
                "export_snapshot",
                "exportSnapshot",
            ],
            Refused(FILES),
        ),
        (
            &["mark"],
            Refused("it writes a mark into the graph's journal"),
        ),
    ]
};

/// The class of the DSL function `name` ([`CLASSES`]); `None` for a name the table does not
/// list.
pub(crate) fn classify(name: &str) -> Option<StepClass> {
    // Outside the table: the name is registered only with the feature.
    #[cfg(feature = "gql")]
    if name == "gql" {
        return Some(StepClass::Gql);
    }
    CLASSES
        .iter()
        .find(|(names, _)| names.contains(&name))
        .map(|(_, class)| *class)
}

/// The effects of a body: walks every expression of `ast` (its function and the closures in
/// it). `Err` is why the body cannot be a saved query (a refused construct, named).
pub(crate) fn analyze(ast: &rhai::AST) -> Result<BodyEffects, String> {
    let mut analysis = Analysis::default();
    ast.walk(&mut |path: &[ASTNode]| {
        let Some(node) = path.last() else {
            return true;
        };
        let parent = path.len().checked_sub(2).and_then(|at| path.get(at));
        match node {
            ASTNode::Stmt(Stmt::Import(..)) => analysis.dynamic("import"),
            // A chain's top: its parent is not a link of the same chain.
            ASTNode::Expr(expr @ Expr::Dot(..))
                if !matches!(parent, Some(ASTNode::Expr(Expr::Dot(..)))) =>
            {
                analysis.chain(expr);
            }
            // A method called like a function (`drop(x)`, `query(g, "q")`): its receiver is the
            // first argument, whose flow is not stated.
            ASTNode::Expr(Expr::FnCall(call, _)) | ASTNode::Stmt(Stmt::FnCall(call, _)) => {
                let args = call.args.get(1..).unwrap_or_default();
                analysis.call(call.name.as_str(), args, &mut AccessFlow::default());
            }
            ASTNode::Expr(Expr::DynamicConstant(value, _)) => {
                if let Some(pointer) = value.read_lock::<rhai::FnPtr>()
                    && !pointer.is_anonymous()
                {
                    analysis.dynamic(&format!("Fn(\"{}\")", pointer.fn_name()));
                }
            }
            _ => {}
        }
        analysis.refused.is_none()
    });
    match analysis.refused {
        Some(reason) => Err(reason),
        None => Ok(analysis.effects),
    }
}

/// The state of one [`analyze`] walk.
#[derive(Default)]
struct Analysis {
    effects: BodyEffects,
    /// Why the body is refused, once known (stops the walk).
    refused: Option<String>,
}

impl Analysis {
    fn dynamic(&mut self, construct: &str) {
        if self.effects.dynamic.is_none() {
            self.effects.dynamic = Some(construct.to_string());
        }
    }

    fn write(&mut self, request: AccessRequest<'_>) {
        let request = request.to_owned_request();
        if !self.effects.writes.contains(&request) {
            self.effects.writes.push(request);
        }
    }

    /// A write of the received elements, the request of `GraphStep::for_each_access_in`.
    fn received(&mut self, flow: &AccessFlow, action: Action<'_>, element: Option<ElementKind>) {
        let element = element.or(flow.element);
        if flow.labels.is_empty() || element != flow.element {
            return self.write(AccessRequest::new(action, Resource::data(element, None)));
        }
        for label in &flow.labels {
            self.write(AccessRequest::new(
                action,
                Resource::data(element, Some(label.as_str())),
            ));
        }
    }

    /// Folds the method chain `top` (`a.b(..).c(..)`, Rhai nests it to the right) link by link.
    fn chain(&mut self, top: &Expr) {
        let mut links = Vec::new();
        flatten(top, &mut links);
        let mut flow = AccessFlow::default();
        // The root (`g`, `__`, a variable, a call) states nothing; a function-style root call is
        // classified when the walk reaches it as an `Expr::FnCall`.
        for link in links.iter().skip(1) {
            match link {
                Expr::MethodCall(call, _) => self.call(call.name.as_str(), &call.args, &mut flow),
                // `a.b()[i].c()` nests as `Dot(a, Index(b(), Dot(i, c())))`: the call before the
                // index is a link of this chain, what follows a chain of its own.
                Expr::Index(binary, _, _) => {
                    if let Expr::MethodCall(call, _) = &binary.lhs {
                        self.call(call.name.as_str(), &call.args, &mut flow);
                    }
                    flow = AccessFlow::default();
                }
                _ => flow = AccessFlow::default(),
            }
        }
    }

    /// One call of the DSL function `name` with `args` (the receiver excluded) on a receiver
    /// whose flow is `flow`; leaves the flow of its result there.
    fn call(&mut self, name: &str, args: &[Expr], flow: &mut AccessFlow) {
        use StepClass::*;
        let Some(class) = classify(name) else {
            *flow = AccessFlow::default();
            return;
        };
        let vertex = Some(ElementKind::Vertex);
        let edge = Some(ElementKind::Edge);
        match class {
            Vertices => *flow = of(ElementKind::Vertex, Vec::new()),
            Edges => *flow = of(ElementKind::Edge, Vec::new()),
            EdgesLabelled => {
                *flow = of(ElementKind::Edge, literal_strings(args).unwrap_or_default());
            }
            AddV | AddE => {
                let element = if class == AddV { vertex } else { edge };
                let labels = literal_strings(args).unwrap_or_default();
                if labels.is_empty() {
                    self.write(AccessRequest::new(
                        Action::Create,
                        Resource::data(element, None),
                    ));
                }
                for label in &labels {
                    self.write(AccessRequest::new(
                        Action::Create,
                        Resource::data(element, Some(label.as_str())),
                    ));
                }
                *flow = AccessFlow { element, labels };
            }
            Merge(kind) => {
                for action in [Action::Create, Action::Update] {
                    self.write(AccessRequest::new(action, Resource::data(Some(kind), None)));
                }
                *flow = of(kind, Vec::new());
            }
            Update => self.received(flow, Action::Update, None),
            Relabel(kind) => {
                self.received(flow, Action::Update, Some(kind));
                flow.labels.clear();
            }
            Drop => self.received(flow, Action::Delete, None),
            HasLabel => {
                if flow.element == edge
                    && flow.labels.is_empty()
                    && let Some(labels) = literal_strings(args).filter(|l| !l.is_empty())
                {
                    flow.labels = labels;
                }
            }
            Keep => {}
            KeepPlain => {
                if !args.iter().all(is_plain) {
                    *flow = AccessFlow::default();
                }
            }
            Query => {
                match args.first() {
                    Some(Expr::StringConstant(name, _)) => {
                        if !self
                            .effects
                            .calls
                            .iter()
                            .any(|known| known == name.as_str())
                        {
                            self.effects.calls.push(name.to_string());
                        }
                    }
                    _ => self.dynamic("g.query(name) with a computed name"),
                }
                *flow = AccessFlow::default();
            }
            #[cfg(feature = "gql")]
            Gql => {
                let planned = match args.first() {
                    Some(Expr::StringConstant(query, _)) => {
                        crate::gql::lower::planned_writes(query.as_str())
                    }
                    _ => None,
                };
                match planned {
                    Some(planned) => {
                        for write in &planned.writes {
                            self.write(write.as_request());
                        }
                        // The saved queries it calls (stage 3h): their effects transitively.
                        for name in planned.calls {
                            if !self.effects.calls.contains(&name) {
                                self.effects.calls.push(name);
                            }
                        }
                        *flow = planned.flow;
                    }
                    None => {
                        for action in [Action::Create, Action::Update, Action::Delete] {
                            self.write(AccessRequest::new(action, Resource::data(None, None)));
                        }
                        *flow = AccessFlow::default();
                    }
                }
            }
            SideEffectTarget => {
                if args.len() > 1 && !args.iter().skip(1).all(is_constant) {
                    self.write(AccessRequest::new(
                        Action::Create,
                        Resource::data(None, None),
                    ));
                }
            }
            Dynamic => {
                self.dynamic(name);
                *flow = AccessFlow::default();
            }
            Refused(why) => {
                if self.refused.is_none() {
                    self.refused = Some(format!(
                        "it calls {name}(), and {why}; a saved query may write data only"
                    ));
                }
            }
        }
    }
}

/// A flow of `element` with `labels`.
fn of(element: ElementKind, labels: Vec<String>) -> AccessFlow {
    AccessFlow {
        element: Some(element),
        labels,
    }
}

/// The links of a method chain in order: Rhai nests `a.b().c()` as `Dot(a, Dot(b(), c()))`.
fn flatten<'a>(expr: &'a Expr, links: &mut Vec<&'a Expr>) {
    match expr {
        Expr::Dot(binary, _, _) => {
            flatten(&binary.lhs, links);
            flatten(&binary.rhs, links);
        }
        other => links.push(other),
    }
}

/// The arguments as string literals (`"a", "b"` or `["a", "b"]`); `None` when one is anything
/// else (a variable, a traversal, a predicate).
fn literal_strings(args: &[Expr]) -> Option<Vec<String>> {
    let mut strings = Vec::new();
    for arg in args {
        match arg {
            Expr::StringConstant(text, _) => strings.push(text.to_string()),
            Expr::Array(items, _) => {
                for item in items.iter() {
                    match item {
                        Expr::StringConstant(text, _) => strings.push(text.to_string()),
                        _ => return None,
                    }
                }
            }
            // A literal array the parser folded into one value.
            Expr::DynamicConstant(value, _) => {
                let items = value.read_lock::<rhai::Array>()?;
                for item in items.iter() {
                    strings.push(item.read_lock::<rhai::ImmutableString>()?.to_string());
                }
            }
            _ => return None,
        }
    }
    Some(strings)
}

/// Whether `arg` is a number or a string literal.
fn is_plain(arg: &Expr) -> bool {
    matches!(
        arg,
        Expr::IntegerConstant(..) | Expr::StringConstant(..) | Expr::FloatConstant(..)
    )
}

/// Whether `arg` is a constant value (no graph can hide in it).
fn is_constant(arg: &Expr) -> bool {
    match arg {
        Expr::IntegerConstant(..)
        | Expr::StringConstant(..)
        | Expr::FloatConstant(..)
        | Expr::BoolConstant(..)
        | Expr::CharConstant(..)
        | Expr::Unit(..) => true,
        Expr::Array(items, _) => items.iter().all(is_constant),
        Expr::Map(map, _) => map.0.iter().all(|(_, value)| is_constant(value)),
        // A literal the parser folded into one value: data, unless a function pointer.
        Expr::DynamicConstant(value, _) => !value.is_fnptr(),
        _ => false,
    }
}

/// Whether the inferred `effect` covers the run-time `request`: the same action on data, of the
/// effect's element kind and label wherever the effect names one (an open kind or label covers
/// every value).
pub(crate) fn covers(effect: &AccessRequestBuf, request: &AccessRequest<'_>) -> bool {
    let effect = effect.as_request();
    let (
        Resource::Data {
            element: effect_element,
            label: effect_label,
        },
        Resource::Data { element, label },
    ) = (effect.resource, request.resource)
    else {
        return false;
    };
    effect.action.kind() == request.action.kind()
        && effect_element.is_none_or(|kind| element == Some(kind))
        && effect_label.is_none_or(|name| label == Some(name))
}

/// Adds `more` to `effects`, each request once.
pub(crate) fn extend(effects: &mut Vec<AccessRequestBuf>, more: &[AccessRequestBuf]) {
    for request in more {
        if !effects.contains(request) {
            effects.push(request.clone());
        }
    }
}

/// What [`transitive`] knows of one saved query: its own effects and whether a caller counts
/// them (a read-only query's writes are denied, so they never reach its caller).
pub(crate) struct Node {
    pub(crate) effects: BodyEffects,
    pub(crate) counts: bool,
}

/// The effects of the saved query `name` with every query it calls, at any depth: its own
/// writes plus those of each called query that counts ([`Node::counts`]). `lookup` gives a
/// query's node (`None`: no such query or one that cannot run, which a call fails on anyway).
/// A cycle is followed once (the call depth bounds it at run time). Also returns whether the
/// body or a counted callee is dynamic.
pub(crate) fn transitive(
    name: &str,
    lookup: &mut dyn FnMut(&str) -> Option<Node>,
) -> (Vec<AccessRequestBuf>, Option<String>) {
    let mut effects = Vec::new();
    let mut dynamic = None;
    let mut seen: HashSet<String> = HashSet::new();
    let mut pending = vec![(name.to_string(), true)];
    while let Some((next, root)) = pending.pop() {
        if !seen.insert(next.clone()) {
            continue;
        }
        let Some(node) = lookup(&next) else {
            continue;
        };
        if !root && !node.counts {
            continue;
        }
        extend(&mut effects, &node.effects.writes);
        if dynamic.is_none() {
            dynamic = node.effects.dynamic.clone();
        }
        pending.extend(node.effects.calls.into_iter().map(|call| (call, false)));
    }
    (effects, dynamic)
}
