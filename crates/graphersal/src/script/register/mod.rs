//! Registration of the DSL with a Rhai engine, by category: the value and graph types
//! (`types.rs`), the token classes (`tokens.rs`), the terminal steps (`terminals.rs`) and the
//! step builders (`steps.rs`), collected once per thread by [`Registry`] (`registry.rs`) and
//! attached to every engine by [`new_engine`].

/// Registers every `GType` token as a property of the `GType` scope marker.
macro_rules! register_gtype_tokens {
    ($engine:expr_2021; $($token:ident),+ $(,)?) => {{
        $( $engine.register_get(stringify!($token), |_: &mut ScriptGType| GType::$token); )+
    }};
}

/// Registers true positional-arity overloads (2..=10 discrete arities) of a Rhai *method* name in
/// the [`Registry`], given a receiver type and a closure that turns the collected
/// `Vec<Dynamic>` into a call on the step's existing array/`Vec<Dynamic>`-taking form (e.g.
/// `s.has_label(args)`).
///
/// # Why this exists
/// Every step in the Overview table of the true-variadic-overloads design mirrors a real
/// TinkerPop `T... varargs` overload (`hasLabel(String, String...)`, `V(Object...)`, ...), but
/// this DSL only ever registered a 0/1-arg form plus a single form taking a Rhai array literal
/// (`hasLabel(["a", "b"])`) — a syntactically valid Gremlin call like `hasLabel("a", "b")` was
/// rejected outright. Rhai's `register_fn` has no native varargs: a call is matched purely by its
/// registered arity, so "true positional variadic" is only reachable by registering enough
/// discrete arities that real queries hit.
///
/// # The cap
/// This macro generates arities 2 through 10 (9 extra arities on top of the pre-existing 0/1/array
/// forms), comfortably above the spec's floor of 6 and well inside Rhai's own per-function
/// ceiling (`rhai::func::register`'s `def_register!` is generated for up to 20 total parameters,
/// including the receiver). 10 was chosen as a practical, generous bound covering every call this
/// codebase's tests and any real Gremlin query are likely to make positionally; a caller building
/// a longer or dynamically-sized list keeps using the **array-literal form**
/// (`step([v1, v2, ..., vN])`), which every wrapped step keeps unchanged as the unlimited-arity
/// escape hatch. Raising the cap further is just adding more arms below, not a design change.
///
/// `$names` is the list of names to register under (`["has_label", "hasLabel"]`: both spellings
/// from one entry, rule 2); `$names => name` binds the name being registered for a delegate that
/// mentions it in diagnostics. `$recv_ty` is the receiver's Rust type (e.g.
/// `&mut ScriptGraphTraversalSource` for a step method, or `ScriptP` for a `P.`-prefixed predicate
/// constructor); `$recv` is its binding name or `_` if unused. `$delegate` is evaluated once per
/// generated arity with `$args` bound to the `Vec<Dynamic>` collected from that arity's positional
/// parameters, in call order.
macro_rules! register_variadic {
    // `ctx` form: the delegate also gets the call's `NativeCallContext` (for the engine's limits).
    (@arity_ctx $registry:expr_2021, $names:expr_2021, [$($p:ident),+], |$ctx:ident, $recv:tt : $recv_ty:ty, $args:ident| $delegate:expr_2021) => {
        for name in $names {
            $registry.register_fn(name, move |$ctx: NativeCallContext, $recv: $recv_ty, $($p: Dynamic),+| {
                let $args: Vec<Dynamic> = vec![$($p),+];
                $delegate
            });
        }
    };
    (ctx $registry:expr_2021, $names:expr_2021, |$ctx:ident, $recv:tt : $recv_ty:ty, $args:ident| $delegate:expr_2021) => {
        register_variadic!(@arity_ctx $registry, $names, [a1, a2], |$ctx, $recv: $recv_ty, $args| $delegate);
        register_variadic!(@arity_ctx $registry, $names, [a1, a2, a3], |$ctx, $recv: $recv_ty, $args| $delegate);
        register_variadic!(@arity_ctx $registry, $names, [a1, a2, a3, a4], |$ctx, $recv: $recv_ty, $args| $delegate);
        register_variadic!(@arity_ctx $registry, $names, [a1, a2, a3, a4, a5], |$ctx, $recv: $recv_ty, $args| $delegate);
        register_variadic!(@arity_ctx $registry, $names, [a1, a2, a3, a4, a5, a6], |$ctx, $recv: $recv_ty, $args| $delegate);
        register_variadic!(@arity_ctx $registry, $names, [a1, a2, a3, a4, a5, a6, a7], |$ctx, $recv: $recv_ty, $args| $delegate);
        register_variadic!(@arity_ctx $registry, $names, [a1, a2, a3, a4, a5, a6, a7, a8], |$ctx, $recv: $recv_ty, $args| $delegate);
        register_variadic!(@arity_ctx $registry, $names, [a1, a2, a3, a4, a5, a6, a7, a8, a9], |$ctx, $recv: $recv_ty, $args| $delegate);
        register_variadic!(@arity_ctx $registry, $names, [a1, a2, a3, a4, a5, a6, a7, a8, a9, a10], |$ctx, $recv: $recv_ty, $args| $delegate);
    };
    (@arity $registry:expr_2021, $names:expr_2021 => $n:ident, [$($p:ident),+], |$recv:tt : $recv_ty:ty, $args:ident| $delegate:expr_2021) => {
        for $n in $names {
            $registry.register_fn($n, move |$recv: $recv_ty, $($p: Dynamic),+| {
                let $args: Vec<Dynamic> = vec![$($p),+];
                $delegate
            });
        }
    };
    ($registry:expr_2021, $names:expr_2021 => $n:ident, |$recv:tt : $recv_ty:ty, $args:ident| $delegate:expr_2021) => {
        register_variadic!(@arity $registry, $names => $n, [a1, a2], |$recv: $recv_ty, $args| $delegate);
        register_variadic!(@arity $registry, $names => $n, [a1, a2, a3], |$recv: $recv_ty, $args| $delegate);
        register_variadic!(@arity $registry, $names => $n, [a1, a2, a3, a4], |$recv: $recv_ty, $args| $delegate);
        register_variadic!(@arity $registry, $names => $n, [a1, a2, a3, a4, a5], |$recv: $recv_ty, $args| $delegate);
        register_variadic!(@arity $registry, $names => $n, [a1, a2, a3, a4, a5, a6], |$recv: $recv_ty, $args| $delegate);
        register_variadic!(@arity $registry, $names => $n, [a1, a2, a3, a4, a5, a6, a7], |$recv: $recv_ty, $args| $delegate);
        register_variadic!(@arity $registry, $names => $n, [a1, a2, a3, a4, a5, a6, a7, a8], |$recv: $recv_ty, $args| $delegate);
        register_variadic!(@arity $registry, $names => $n, [a1, a2, a3, a4, a5, a6, a7, a8, a9], |$recv: $recv_ty, $args| $delegate);
        register_variadic!(@arity $registry, $names => $n, [a1, a2, a3, a4, a5, a6, a7, a8, a9, a10], |$recv: $recv_ty, $args| $delegate);
    };
    ($registry:expr_2021, $names:expr_2021, |$recv:tt : $recv_ty:ty, $args:ident| $delegate:expr_2021) => {
        register_variadic!($registry, $names => _name, |$recv: $recv_ty, $args| $delegate)
    };
}

/// [`register_variadic!`]'s counterpart for a Rhai function registered directly into a `Module`
/// (via `FuncRegistration::set_into_module`) rather than into the [`Registry`], and with no
/// receiver parameter at all — used for `P::within`/`P::without` (and their bare-global
/// reach-through via the same shared module), which are free functions, not methods. Same arity
/// cap and rationale as [`register_variadic!`]; `$names` is a list of names as well.
macro_rules! register_variadic_module {
    (@arity $module:expr_2021, $names:expr_2021, [$($p:ident),+], |$args:ident| $delegate:expr_2021) => {
        for name in $names {
            FuncRegistration::new(name).set_into_module($module, |$($p: Dynamic),+| {
                let $args: Vec<Dynamic> = vec![$($p),+];
                $delegate
            });
        }
    };
    ($module:expr_2021, $names:expr_2021, |$args:ident| $delegate:expr_2021) => {
        register_variadic_module!(@arity $module, $names, [a1, a2], |$args| $delegate);
        register_variadic_module!(@arity $module, $names, [a1, a2, a3], |$args| $delegate);
        register_variadic_module!(@arity $module, $names, [a1, a2, a3, a4], |$args| $delegate);
        register_variadic_module!(@arity $module, $names, [a1, a2, a3, a4, a5], |$args| $delegate);
        register_variadic_module!(@arity $module, $names, [a1, a2, a3, a4, a5, a6], |$args| $delegate);
        register_variadic_module!(@arity $module, $names, [a1, a2, a3, a4, a5, a6, a7], |$args| $delegate);
        register_variadic_module!(@arity $module, $names, [a1, a2, a3, a4, a5, a6, a7, a8], |$args| $delegate);
        register_variadic_module!(@arity $module, $names, [a1, a2, a3, a4, a5, a6, a7, a8, a9], |$args| $delegate);
        register_variadic_module!(@arity $module, $names, [a1, a2, a3, a4, a5, a6, a7, a8, a9, a10], |$args| $delegate);
    };
}

mod docs;

/// Every DSL function's documented example as `(receiver, name, example, result)`, the
/// catalogue of `docs.rs`: `result` is the documented `to_string()` text (empty when it is not
/// shown, `error: <text>` for a documented failure), `None` for an example the docs test does
/// not run (it needs a file or a host setup). For the storage differential in the integration
/// tests (`tests/all/foreign_storage_tests.rs`); not a stable API.
#[doc(hidden)]
pub fn doc_examples() -> impl Iterator<
    Item = (
        &'static str,
        &'static str,
        &'static str,
        Option<&'static str>,
    ),
> {
    docs::DOCS.iter().map(|doc| {
        let result = (doc.result != docs::NOT_RUN).then_some(doc.result);
        (doc.receiver, doc.name, doc.example, result)
    })
}

// The checks read the metadata as JSON (`serde_json` comes with `serde`).
#[cfg(all(test, feature = "serde"))]
mod docs_tests;
mod registry;
mod steps;
mod terminals;
mod tokens;
mod types;

pub(crate) use registry::Registry;
pub(super) use registry::new_engine;
// Read only by `surface_tests` (which needs `io` and `serde`).
#[cfg(all(test, feature = "io", feature = "serde"))]
pub(crate) use registry::replaced_signatures;
pub(crate) use registry::{display_type_name, display_type_names_in};

/// Registers every DSL type, token class and step into `registry`, which is built once per
/// thread and shared by every engine ([`new_engine`]). Types are registered before the functions
/// that take them, so the parameter metadata carries their display names.
fn register_all(registry: &mut Registry) {
    types::register_graph_source(registry);
    tokens::register(registry);
    crate::script::tokens::register_element_types(registry);
    types::register_values(registry);
    types::register_schema(registry);
    terminals::register(registry);
    steps::register(registry);
    crate::script::fake::register(registry);
}
