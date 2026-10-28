//! The registration target of the DSL: [`Registry`] collects every call-independent
//! registration (functions, getters, indexers, iterators, custom type names, the static token
//! modules and the extra global modules) into plain `rhai::Module`s, built ONCE per thread
//! ([`new_engine`]); every new engine then attaches them by pointer clone instead of re-running
//! ~2000 registrations.
//!
//! [`Registry`] deliberately mirrors the `Engine` registration API (`register_fn`,
//! `register_get`, `register_type_with_name`, `register_static_module`, ...) and produces the
//! same function metadata the engine would (parameter types formatted with the display names
//! registered so far), so the registered surface is identical to registering on each engine
//! (checked by `surface_tests.rs`).

use std::any::TypeId;
#[cfg(all(test, feature = "io", feature = "serde"))]
use std::collections::HashSet;

use rhai::packages::{Package, StandardPackage};
use rhai::{Engine, FuncRegistration, Module, RhaiNativeFunc, Shared, Variant};

/// Collects the DSL registrations into shared modules. See the module docs.
pub(crate) struct Registry {
    /// Functions, getters, indexers, iterators and custom type names: what `Engine::register_*`
    /// puts into the engine's own global namespace.
    namespace: Module,
    /// The extra global modules (`register_global_module`), in registration order.
    globals: Vec<Shared<Module>>,
    /// The static modules (`register_static_module`), in registration order, already indexed.
    statics: Vec<(&'static str, Shared<Module>)>,
    /// A bare engine that mirrors the custom type names and modules registered so far, only to
    /// format the parameter types of the metadata exactly as `Engine::register_fn` does.
    names: Engine,
    /// The function documentation, attached to every registered function (`docs.rs`).
    docs: super::docs::DocIndex,
    /// Every registered `(name, parameter types)`, to report a registration that replaces an
    /// earlier one with the same signature (the later one wins, so their order would matter).
    #[cfg(all(test, feature = "io", feature = "serde"))]
    seen: HashSet<(String, Vec<TypeId>)>,
    /// The replaced signatures (see `seen`); a test keeps it empty.
    #[cfg(all(test, feature = "io", feature = "serde"))]
    replaced: Vec<String>,
}

/// The finished registrations: what [`new_engine`] attaches to every engine.
pub(crate) struct SharedRegistrations {
    /// Rhai's standard library (`Engine::new` builds it anew for every engine, which was most
    /// of the remaining construction time).
    standard: Shared<Module>,
    namespace: Shared<Module>,
    globals: Vec<Shared<Module>>,
    statics: Vec<(&'static str, Shared<Module>)>,
    /// The replaced signatures found while building (see [`Registry`]).
    #[cfg(all(test, feature = "io", feature = "serde"))]
    replaced: Vec<String>,
}

impl Registry {
    fn new() -> Self {
        Self {
            namespace: Module::new(),
            globals: Vec::new(),
            statics: Vec::new(),
            names: Engine::new_raw(),
            docs: super::docs::DocIndex::new(),
            #[cfg(all(test, feature = "io", feature = "serde"))]
            seen: HashSet::new(),
            #[cfg(all(test, feature = "io", feature = "serde"))]
            replaced: Vec::new(),
        }
    }

    /// `Engine::format_param_type`: the display name of a Rust parameter type, keeping `&mut `.
    fn param_type(&self, ty: &str) -> String {
        if let Some(inner) = ty.strip_prefix("&mut ") {
            let mapped = self.param_type(inner);
            return if mapped == inner {
                ty.to_string()
            } else {
                format!("&mut {mapped}")
            };
        }
        self.names.map_type_name(ty).to_string()
    }

    /// `Engine::register_fn`: a native function in the global namespace, with the parameter
    /// type metadata the engine would record.
    pub(crate) fn register_fn<A: 'static, const N: usize, const X: bool, R, const F: bool, FUNC>(
        &mut self,
        name: impl AsRef<str>,
        func: FUNC,
    ) -> &mut Self
    where
        R: Variant + Clone,
        FUNC: RhaiNativeFunc<A, N, X, R, F> + 'static,
    {
        let name = name.as_ref();
        #[cfg(all(test, feature = "io", feature = "serde"))]
        if !self
            .seen
            .insert((name.to_string(), FUNC::param_types().to_vec()))
        {
            self.replaced
                .push(format!("{name}{:?}", FUNC::param_names()));
        }
        let mut params: Vec<String> = FUNC::param_names()
            .iter()
            .map(|ty| format!("_: {}", self.param_type(ty)))
            .collect();
        if FUNC::return_type() != TypeId::of::<()>() {
            params.push(self.param_type(FUNC::return_type_name()));
        }
        let receiver = super::docs::receiver_of(FUNC::param_names().first().copied());
        let comments = self.docs.get(receiver, name);
        FuncRegistration::new(name)
            .with_params_info(params)
            .with_comments(comments)
            .in_global_namespace()
            .set_into_module(&mut self.namespace, func);
        self
    }

    /// The doc comment lines of `name` called on `receiver` (`docs.rs`), for a function set
    /// directly into a module (`FuncRegistration::with_comments`) instead of [`Self::register_fn`].
    pub(crate) fn doc_comments(&mut self, receiver: &'static str, name: &str) -> Vec<String> {
        self.docs.get(receiver, name)
    }

    /// [`register_fn`](Self::register_fn) under several names: the snake_case and camelCase
    /// spellings of one step (rule 2), registered from ONE entry so neither can be forgotten.
    pub(crate) fn register_fn_as<
        A: 'static,
        const N: usize,
        const X: bool,
        R,
        const F: bool,
        FUNC,
    >(
        &mut self,
        names: &[&str],
        func: FUNC,
    ) -> &mut Self
    where
        R: Variant + Clone,
        FUNC: RhaiNativeFunc<A, N, X, R, F> + Clone + 'static,
    {
        for name in names {
            self.register_fn(name, func.clone());
        }
        self
    }

    /// `Engine::register_get`: a property getter (`x.name`).
    pub(crate) fn register_get<A: 'static, const X: bool, R, const F: bool, FUNC>(
        &mut self,
        name: &str,
        func: FUNC,
    ) -> &mut Self
    where
        R: Variant + Clone,
        FUNC: RhaiNativeFunc<A, 1, X, R, F> + 'static,
    {
        self.register_fn(format!("get${name}"), func)
    }

    /// `Engine::register_indexer_get`: an index getter (`x[key]`).
    pub(crate) fn register_indexer_get<A: 'static, const X: bool, R, const F: bool, FUNC>(
        &mut self,
        func: FUNC,
    ) -> &mut Self
    where
        R: Variant + Clone,
        FUNC: RhaiNativeFunc<A, 2, X, R, F> + 'static,
    {
        self.register_fn("index$get$", func)
    }

    /// `Engine::register_iterator`: `for x in value` over a custom type.
    pub(crate) fn register_iterator<T>(&mut self) -> &mut Self
    where
        T: Variant + Clone + IntoIterator,
        <T as IntoIterator>::Item: Variant + Clone,
    {
        self.namespace.set_iterable::<T>();
        self
    }

    /// `Engine::register_type_with_name`: the script display name of a custom type (`type_of`,
    /// diagnostics, metadata).
    pub(crate) fn register_type_with_name<T: Variant + Clone>(&mut self, name: &str) -> &mut Self {
        self.namespace.set_custom_type::<T>(name);
        self.names.register_type_with_name::<T>(name);
        self
    }

    /// `Engine::register_static_module`: the `name::x` namespace. Indexed here, once, so that
    /// attaching it to an engine is a pointer clone.
    pub(crate) fn register_static_module(
        &mut self,
        name: &'static str,
        module: Shared<Module>,
    ) -> &mut Self {
        let mut module = Shared::try_unwrap(module).unwrap_or_else(|shared| (*shared).clone());
        module.build_index();
        if module.id().is_none() {
            module.set_id(name);
        }
        let module: Shared<Module> = module.into();
        self.names.register_static_module(name, module.clone());
        self.statics.push((name, module));
        self
    }

    /// `Engine::register_global_module`: functions callable without a namespace.
    pub(crate) fn register_global_module(&mut self, module: Shared<Module>) -> &mut Self {
        self.names.register_global_module(module.clone());
        self.globals.push(module);
        self
    }

    fn finish(self) -> SharedRegistrations {
        SharedRegistrations {
            standard: StandardPackage::new().as_shared_module(),
            namespace: self.namespace.into(),
            globals: self.globals,
            statics: self.statics,
            #[cfg(all(test, feature = "io", feature = "serde"))]
            replaced: self.replaced,
        }
    }
}

thread_local! {
    /// The registrations of this thread, built on first use. A `thread_local!` because `Shared`
    /// is an `Rc` without Rhai's `sync` feature; wasm32 has one thread anyway.
    static SHARED: SharedRegistrations = {
        let mut registry = Registry::new();
        super::register_all(&mut registry);
        registry.finish()
    };
}

/// A new engine with Rhai's standard library and every DSL registration attached by pointer
/// clone: `Engine::new()` without rebuilding the standard library, plus the DSL.
///
/// It reproduces `Engine::new()` (rhai 1.26) on top of `Engine::new_raw()`: the default
/// `print`/`debug` output (not on wasm32-unknown-unknown, like Rhai), the string interner and
/// the standard package. The module resolver of `import` is NOT Rhai's default file resolver:
/// `engine_with_limits` installs one that asks the host's `Authorizer` (`Execute` on the
/// module's `File`, see `script/access.rs`). The global modules keep the precedence
/// they had when registered on the engine one by one: `register_global_module` inserts in front
/// of the earlier ones, so the standard library goes first (searched last), then the extra
/// modules, and the DSL namespace last (searched first).
pub(crate) fn new_engine() -> Engine {
    let mut engine = Engine::new_raw();

    #[cfg(any(not(target_family = "wasm"), not(target_os = "unknown")))]
    {
        // Rhai's default `print`/`debug` targets, written without panicking on a closed stdout.
        engine.on_print(|text| {
            use std::io::Write as _;
            let _ = writeln!(std::io::stdout(), "{text}");
        });
        engine.on_debug(|text, source, pos| {
            use std::io::Write as _;
            let mut out = std::io::stdout();
            let _ = match (source, pos) {
                (Some(source), rhai::Position::NONE) => writeln!(out, "{source} | {text}"),
                (Some(source), pos) => writeln!(out, "{source} @ {pos:?} | {text}"),
                (None, rhai::Position::NONE) => writeln!(out, "{text}"),
                (None, pos) => writeln!(out, "{pos:?} | {text}"),
            };
        });
    }
    engine.set_max_strings_interned(rhai::default_limits::MAX_STRINGS_INTERNED);

    SHARED.with(|shared| {
        engine.register_global_module(shared.standard.clone());
        for module in &shared.globals {
            engine.register_global_module(module.clone());
        }
        engine.register_global_module(shared.namespace.clone());
        for (name, module) in &shared.statics {
            engine.register_static_module(*name, module.clone());
        }
    });
    engine
}

/// The script display name of a Rust type name as `Dynamic::type_name()` or Rhai's own errors
/// report it: the name a custom type is registered under (`Predicate`, `GraphTraversalSource`,
/// `Operator`), the value type names of the engine for Rhai's primitives (`int64`, `float64`,
/// `boolean`, `string`), Rhai's names for its containers (`array`, `map`). A user-facing text
/// never shows a Rust type path (`graphersal::script::tokens::predicate::ArgP`) or a Rust
/// primitive (`f64`); an unregistered type shows its last path segment.
pub(crate) fn display_type_name(raw: &str) -> String {
    let raw = raw.trim();
    let primitive = match raw {
        "i64" | "i32" | "INT" | "u64" | "usize" => Some("int64"),
        "f64" | "f32" | "FLOAT" => Some("float64"),
        "bool" => Some("boolean"),
        "string"
        | "&str"
        | "String"
        | "ImmutableString"
        | "&str | ImmutableString | String"
        | "rhai::ImmutableString"
        | "alloc::string::String" => Some("string"),
        "()" => Some("()"),
        "array" | "rhai::Array" | "Array" => Some("array"),
        "map" | "rhai::Map" | "Map" => Some("map"),
        _ => None,
    };
    if let Some(name) = primitive {
        return name.to_string();
    }
    if let Some(inner) = raw.strip_prefix("&mut ") {
        return display_type_name(inner);
    }
    if raw == std::any::type_name::<crate::script::ScriptUuid>() {
        // A UUID literal is a value: its value type name.
        return "uuid".to_string();
    }
    if raw == std::any::type_name::<crate::script::ScriptVector>() {
        // A vector is a value: its value type name.
        return "vector".to_string();
    }
    let registered = SHARED
        .try_with(|shared| {
            shared
                .namespace
                .get_custom_type_display_by_name(raw)
                .map(str::to_string)
        })
        .ok()
        .flatten();
    match registered {
        Some(name) => name,
        None if raw.contains("::") => {
            let last = raw.rsplit("::").next().unwrap_or(raw);
            last.strip_prefix("Script").unwrap_or(last).to_string()
        }
        None => raw.to_string(),
    }
}

/// Every Rust type path (`graphersal::...::Name`, `alloc::...`) and Rhai primitive name in a
/// call signature or type list of a Rhai error (`to (i64, graphersal::value::merge::Merge)`)
/// replaced by its [`display_type_name`].
pub(crate) fn display_type_names_in(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        if !token.is_empty() {
            let mapped = if token.contains("::")
                || matches!(token.as_str(), "i64" | "f64" | "bool" | "ImmutableString")
            {
                display_type_name(token)
            } else {
                token.clone()
            };
            out.push_str(&mapped);
            token.clear();
        }
    };
    for c in text.chars() {
        if c.is_alphanumeric() || c == '_' || c == ':' {
            token.push(c);
        } else {
            flush(&mut token, &mut out);
            out.push(c);
        }
    }
    flush(&mut token, &mut out);
    out.replace("&str | string | String", "string")
}

/// The replaced signatures of this thread's registrations (see [`Registry`]).
#[cfg(all(test, feature = "io", feature = "serde"))]
pub(crate) fn replaced_signatures() -> Vec<String> {
    SHARED.with(|shared| shared.replaced.clone())
}
