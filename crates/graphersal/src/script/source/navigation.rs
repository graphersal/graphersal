//! Builder methods of adjacency, string, cast, label, select, loop and branch steps
//! (`out`, `inE`, `toUpper`, `asString`, `as`, `select`, `repeat`, `choose`, `option`, ...).

use crate::script::*;

impl ScriptGraphTraversalSource {
    /// Moves to the adjacent vertices over outgoing edges, optionally only edges with the given
    /// labels. Rhai binding of `out()`, the overload without arguments.
    pub fn out_empty(&mut self) -> Self {
        self.add_step(Step::Out(vec![]))
    }

    /// Moves to the adjacent vertices over outgoing edges, optionally only edges with the given
    /// labels. Rhai binding of `out()`, the overload with one argument.
    pub fn out_single(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::Out(vec_dynamic_to_compact_string("out", vec![arg])?)))
    }

    /// Moves to the adjacent vertices over outgoing edges, optionally only edges with the given
    /// labels. Rhai binding of `out()`.
    pub fn out(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::Out(vec_dynamic_to_compact_string("out", arg)?)))
    }

    /// Moves to the adjacent vertices over incoming edges, optionally only edges with the given
    /// labels. Rhai binding of `in()`, the overload without arguments.
    pub fn in_empty(&mut self) -> Self {
        self.add_step(Step::In(vec![]))
    }

    /// Moves to the adjacent vertices over incoming edges, optionally only edges with the given
    /// labels. Rhai binding of `in()`, the overload with one argument.
    pub fn in_single(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::In(vec_dynamic_to_compact_string("in", vec![arg])?)))
    }

    /// Moves to the adjacent vertices over incoming edges, optionally only edges with the given
    /// labels. Rhai binding of `in()`.
    pub fn in_(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::In(vec_dynamic_to_compact_string("in", arg)?)))
    }

    /// Moves from a property to the vertex or edge that owns it. Rhai binding of `element()`.
    pub fn element(&mut self) -> Self {
        self.add_step(Step::Element)
    }

    /// Yields the length of each string in characters (Unicode scalar values, see the book page
    /// Type Conversion and length()). Rhai binding of `length()`.
    pub fn length(&mut self) -> Self {
        self.add_step(Step::Length)
    }

    /// Moves to the adjacent vertices over incoming and outgoing edges, optionally only edges with
    /// the given labels. Rhai binding of `both()`, the overload without arguments.
    pub fn both_empty(&mut self) -> Self {
        self.add_step(Step::Both(vec![]))
    }

    /// Moves to the adjacent vertices over incoming and outgoing edges, optionally only edges with
    /// the given labels. Rhai binding of `both()`, the overload with one argument.
    pub fn both_single(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::Both(vec_dynamic_to_compact_string(
            "both",
            vec![arg],
        )?)))
    }

    /// Moves to the adjacent vertices over incoming and outgoing edges, optionally only edges with
    /// the given labels. Rhai binding of `both()`.
    pub fn both(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::Both(vec_dynamic_to_compact_string("both", arg)?)))
    }

    /// Moves to the incident edges in both directions, optionally only edges with the given labels.
    /// Rhai binding of `both_e()`, the overload without arguments.
    pub fn both_e_empty(&mut self) -> Self {
        self.add_step(Step::BothE(vec![]))
    }

    /// Moves to the incident edges in both directions, optionally only edges with the given labels.
    /// Rhai binding of `both_e()`, the overload with one argument.
    pub fn both_e_single(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::BothE(vec_dynamic_to_compact_string(
            "both_e",
            vec![arg],
        )?)))
    }

    /// Moves to the incident edges in both directions, optionally only edges with the given labels.
    /// Rhai binding of `both_e()`.
    pub fn both_e(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::BothE(vec_dynamic_to_compact_string("both_e", arg)?)))
    }

    /// Moves to the incoming edges, optionally only edges with the given labels. Rhai binding of
    /// `in_e()`, the overload without arguments.
    pub fn in_e_empty(&mut self) -> Self {
        self.add_step(Step::InE(vec![]))
    }

    /// Moves to the incoming edges, optionally only edges with the given labels. Rhai binding of
    /// `in_e()`, the overload with one argument.
    pub fn in_e_single(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::InE(vec_dynamic_to_compact_string("in_e", vec![arg])?)))
    }

    /// Moves to the incoming edges, optionally only edges with the given labels. Rhai binding of
    /// `in_e()`.
    pub fn in_e(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::InE(vec_dynamic_to_compact_string("in_e", arg)?)))
    }

    /// Moves from an edge to its in (head) vertex. Rhai binding of `in_v()`.
    pub fn in_v(&mut self) -> Self {
        self.add_step(Step::InV)
    }

    /// Moves to the outgoing edges, optionally only edges with the given labels. Rhai binding of
    /// `out_e()`, the overload without arguments.
    pub fn out_e_empty(&mut self) -> Self {
        self.add_step(Step::OutE(vec![]))
    }

    /// Moves to the outgoing edges, optionally only edges with the given labels. Rhai binding of
    /// `out_e()`, the overload with one argument.
    pub fn out_e_single(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::OutE(vec_dynamic_to_compact_string(
            "out_e",
            vec![arg],
        )?)))
    }

    /// Moves to the outgoing edges, optionally only edges with the given labels. Rhai binding of
    /// `out_e()`.
    pub fn out_e(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::OutE(vec_dynamic_to_compact_string("out_e", arg)?)))
    }

    /// Moves from an edge to its out (tail) vertex. Rhai binding of `out_v()`.
    pub fn out_v(&mut self) -> Self {
        self.add_step(Step::OutV)
    }

    /// Moves from an edge to both of its endpoint vertices (out vertex first). Rhai binding of
    /// `both_v()`.
    pub fn both_v(&mut self) -> Self {
        self.add_step(Step::BothV)
    }

    /// Moves from an edge to the vertex at the other end than the one the traverser came from. Rhai
    /// binding of `other_v()`.
    pub fn other_v(&mut self) -> Self {
        self.add_step(Step::OtherV)
    }

    /// Replaces every incoming value with the given constant. Rhai binding of `constant()`.
    pub fn constant(
        ctx: NativeCallContext,
        this: &mut Self,
        arg: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let value = value_arg(&ctx, arg)?;
        Ok(this.add_step(Step::Constant(value)))
    }

    /// Removes leading and trailing whitespace (Unicode White_Space, see the book page Type
    /// Conversion and length()) from each string. Rhai binding of `trim()`.
    pub fn trim(&mut self) -> Self {
        self.add_step(Step::Trim)
    }

    /// Removes leading whitespace (Unicode White_Space) from each string. Rhai binding of
    /// `l_trim()`.
    pub fn l_trim(&mut self) -> Self {
        self.add_step(Step::LTrim)
    }

    /// Removes trailing whitespace (Unicode White_Space) from each string. Rhai binding of
    /// `r_trim()`.
    pub fn r_trim(&mut self) -> Self {
        self.add_step(Step::RTrim)
    }

    /// Converts each string to lower case. Rhai binding of `to_lower()`.
    pub fn to_lower(&mut self) -> Self {
        self.add_step(Step::ToLower)
    }

    /// Converts each string to upper case. Rhai binding of `to_upper()`.
    pub fn to_upper(&mut self) -> Self {
        self.add_step(Step::ToUpper)
    }

    /// Converts each value to its string text (Scope.local: each element of a list); a list itself
    /// is a cast error. Rhai binding of `as_string()`.
    pub fn as_string(&mut self) -> Self {
        self.add_step(Step::AsString(CastPolicy::default()))
    }

    /// Converts each value to its string text (Scope.local: each element of a list); a list itself
    /// is a cast error. Rhai binding of `as_string()`, the overload with a `CastPolicy`.
    pub fn as_string_with(&mut self, policy: CastPolicy) -> Self {
        self.add_step(Step::AsString(policy))
    }

    /// Converts the value to a boolean: true/false strings in any case, numbers (0 is false);
    /// anything else is an error unless a CastPolicy says otherwise. Rhai binding of `as_bool()`.
    pub fn as_bool(&mut self) -> Self {
        self.add_step(Step::AsBool(CastPolicy::default()))
    }

    /// Converts the value to a boolean: true/false strings in any case, numbers (0 is false);
    /// anything else is an error unless a CastPolicy says otherwise. Rhai binding of `as_bool()`,
    /// the overload with a `CastPolicy`.
    pub fn as_bool_with(&mut self, policy: CastPolicy) -> Self {
        self.add_step(Step::AsBool(policy))
    }

    /// Converts the value to a number, optionally of the given GType (GType.LONG truncates toward
    /// zero, see the book page Type Conversion and length()). Rhai binding of `as_number()`.
    pub fn as_number(&mut self) -> Self {
        self.add_step(Step::AsNumber(None, CastPolicy::default()))
    }

    /// Converts the value to a number, optionally of the given GType (GType.LONG truncates toward
    /// zero, see the book page Type Conversion and length()). Rhai binding of `as_number()`, the
    /// overload with a target `GType`.
    pub fn as_number_t(&mut self, target: GType) -> Self {
        self.add_step(Step::AsNumber(Some(target), CastPolicy::default()))
    }

    /// Converts the value to a number, optionally of the given GType (GType.LONG truncates toward
    /// zero, see the book page Type Conversion and length()). Rhai binding of `as_number()`, the
    /// overload with a `CastPolicy`.
    pub fn as_number_with(&mut self, target: GType, policy: CastPolicy) -> Self {
        self.add_step(Step::AsNumber(Some(target), policy))
    }

    /// Converts the value to the given GType (number, string, boolean, uuid, list, map), optionally
    /// with a CastPolicy for values that do not convert. Rhai binding of `cast()`.
    pub fn cast(&mut self, target: GType) -> Self {
        self.add_step(Step::Cast(target, CastPolicy::default()))
    }

    /// Converts the value to the given GType (number, string, boolean, uuid, list, map), optionally
    /// with a CastPolicy for values that do not convert. Rhai binding of `cast()`, the overload
    /// with a `CastPolicy`.
    pub fn cast_with(&mut self, target: GType, policy: CastPolicy) -> Self {
        self.add_step(Step::Cast(target, policy))
    }

    /// Yields the part of each string from start (to end, exclusive); negative positions count from
    /// the end. Rhai binding of `substring()`.
    pub fn substring(&mut self, a_from: i64, a_to: i64) -> Self {
        self.add_step(Step::Substring(a_from as i32, a_to as i32))
    }

    /// `substring(start)`: from `start` (negative counts from the end) to the end of the string.
    pub fn substring_from(&mut self, a_from: i64) -> Self {
        self.add_step(Step::SubstringScoped(
            TraversalScope::Global,
            clamp_i32(a_from),
            None,
        ))
    }

    /// `substring(Scope, start)`; see [`crate::GraphTraversalSource::substring_scoped`].
    pub fn substring_scoped_from(&mut self, scope: TraversalScope, a_from: i64) -> Self {
        self.add_step(Step::SubstringScoped(scope, clamp_i32(a_from), None))
    }

    /// `substring(Scope, start, end)`; see [`crate::GraphTraversalSource::substring_scoped`].
    pub fn substring_scoped(&mut self, scope: TraversalScope, a_from: i64, a_to: i64) -> Self {
        self.add_step(Step::SubstringScoped(
            scope,
            clamp_i32(a_from),
            Some(clamp_i32(a_to)),
        ))
    }

    /// `toUpper(Scope)` / `to_upper(Scope)`; see [`crate::GraphTraversalSource::to_upper_scoped`].
    pub fn to_upper_scoped(&mut self, scope: TraversalScope) -> Self {
        self.add_step(Step::ToUpperScoped(scope))
    }

    /// `toLower(Scope)` / `to_lower(Scope)`; see [`crate::GraphTraversalSource::to_lower_scoped`].
    pub fn to_lower_scoped(&mut self, scope: TraversalScope) -> Self {
        self.add_step(Step::ToLowerScoped(scope))
    }

    /// `trim(Scope)`; see [`crate::GraphTraversalSource::trim_scoped`].
    pub fn trim_scoped(&mut self, scope: TraversalScope) -> Self {
        self.add_step(Step::TrimScoped(scope))
    }

    /// `lTrim(Scope)` / `l_trim(Scope)`; see [`crate::GraphTraversalSource::l_trim_scoped`].
    pub fn l_trim_scoped(&mut self, scope: TraversalScope) -> Self {
        self.add_step(Step::LTrimScoped(scope))
    }

    /// `rTrim(Scope)` / `r_trim(Scope)`; see [`crate::GraphTraversalSource::r_trim_scoped`].
    pub fn r_trim_scoped(&mut self, scope: TraversalScope) -> Self {
        self.add_step(Step::RTrimScoped(scope))
    }

    /// `length(Scope)`; see [`crate::GraphTraversalSource::length_scoped`].
    pub fn length_scoped(&mut self, scope: TraversalScope) -> Self {
        self.add_step(Step::LengthScoped(scope))
    }

    /// `asString(Scope)` / `as_string(Scope)`; see
    /// [`crate::GraphTraversalSource::as_string_scoped`].
    pub fn as_string_scoped(&mut self, scope: TraversalScope) -> Self {
        self.add_step(Step::AsStringScoped(scope))
    }

    /// `replace(Scope, from, to)`; see [`crate::GraphTraversalSource::replace_scoped`].
    pub fn replace_scoped(
        &mut self,
        scope: TraversalScope,
        str_from: Dynamic,
        str_to: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let str_from = nullable_string("replace", str_from)?;
        let str_to = nullable_string("replace", str_to)?;
        Ok(self.add_step(Step::ReplaceScoped(scope, str_from, str_to)))
    }

    /// Labels the current step so select(), where() and path() can refer back to it. Rhai binding
    /// of `as()`, the overload with one argument.
    pub fn as_single(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::As(vec_dynamic_to_compact_string("as", vec![arg])?)))
    }

    /// Labels the current step so select(), where() and path() can refer back to it. Rhai binding
    /// of `as()`.
    pub fn as_(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::As(vec_dynamic_to_compact_string("as", arg)?)))
    }

    /// Sets the out vertex of the edge add_e() creates: a vertex id, a path label or a child
    /// traversal; for path() it starts the window at a label. Rhai binding of `from()`, the
    /// overload with one argument.
    pub fn from_single(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        if arg.is::<ScriptGraphTraversalSource>() {
            return Ok(self.add_step(Step::FromTraversal(try_dynamic_to_gts(arg)?)));
        }
        Ok(self.add_step(Step::From(vec![name_arg("from", arg)?])))
    }

    /// Sets the out vertex of the edge add_e() creates: a vertex id, a path label or a child
    /// traversal; for path() it starts the window at a label. Rhai binding of `from()`.
    pub fn from_step(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::From(vec_dynamic_to_compact_string("from", arg)?)))
    }

    /// `to(Direction)`: `out()`/`in()`/`both()` without a label filter; see
    /// [`crate::GraphTraversalSource::to_direction`].
    pub fn to_direction(&mut self, direction: Direction) -> Self {
        self.direction_step(direction, Vec::new(), DirectionStepKind::Vertex)
    }

    /// `toE(Direction)`: `outE()`/`inE()`/`bothE()`; see [`crate::GraphTraversalSource::to_e`].
    pub fn to_e(&mut self, direction: Direction) -> Self {
        self.direction_step(direction, Vec::new(), DirectionStepKind::Edge)
    }

    /// `toV(Direction)`: `outV()`/`inV()`/`bothV()`; see [`crate::GraphTraversalSource::to_v`].
    pub fn to_v(&mut self, direction: Direction) -> Self {
        match direction {
            Direction::Out => self.out_v(),
            Direction::In => self.in_v(),
            Direction::Both => self.both_v(),
        }
    }

    /// `to(Direction, label, ...)` / `to(Direction, [labels])` (and `toE(..)` with
    /// `edges == true`): the first argument must be a `Direction`, the rest label strings or
    /// arrays of them. A first argument that is not a `Direction` is reported as a missing
    /// overload, so `to(..)`'s add-edge forms keep their own error text.
    pub(crate) fn direction_args_step(
        &mut self,
        name: &str,
        args: Vec<Dynamic>,
        kind: DirectionStepKind,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let signature = format!(
            "{name} ({})",
            args.iter().map(type_name_of).collect::<Vec<_>>().join(", ")
        );
        let mut args = args.into_iter();
        let Some(direction) = args.next().and_then(|arg| arg.try_cast::<Direction>()) else {
            return Err(Box::new(rhai::EvalAltResult::ErrorFunctionNotFound(
                signature,
                rhai::Position::NONE,
            )));
        };
        let mut labels = Vec::new();
        for arg in args {
            if arg.is_array() {
                labels.extend(vec_dynamic_to_compact_string(
                    name,
                    arg.cast::<rhai::Array>(),
                )?);
            } else {
                labels.push(name_arg(name, arg)?);
            }
        }
        Ok(self.direction_step(direction, labels, kind))
    }

    fn direction_step(
        &mut self,
        direction: Direction,
        labels: Vec<CompactString>,
        kind: DirectionStepKind,
    ) -> Self {
        let step = match (kind, direction) {
            (DirectionStepKind::Vertex, Direction::Out) => Step::Out(labels),
            (DirectionStepKind::Vertex, Direction::In) => Step::In(labels),
            (DirectionStepKind::Vertex, Direction::Both) => Step::Both(labels),
            (DirectionStepKind::Edge, Direction::Out) => Step::OutE(labels),
            (DirectionStepKind::Edge, Direction::In) => Step::InE(labels),
            (DirectionStepKind::Edge, Direction::Both) => Step::BothE(labels),
        };
        self.add_step(step)
    }

    /// Sets the in vertex of the edge add_e() creates (vertex id, path label or child traversal);
    /// with a Direction it moves like out()/in()/both(). Rhai binding of `to()`, the overload with
    /// one argument.
    pub fn to_single(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        if arg.is::<ScriptGraphTraversalSource>() {
            return Ok(self.add_step(Step::ToTraversal(try_dynamic_to_gts(arg)?)));
        }
        Ok(self.add_step(Step::To(vec![name_arg("to", arg)?])))
    }

    /// Sets the in vertex of the edge add_e() creates (vertex id, path label or child traversal);
    /// with a Direction it moves like out()/in()/both(). Rhai binding of `to()`.
    pub fn to(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        Ok(self.add_step(Step::To(vec_dynamic_to_compact_string("to", arg)?)))
    }

    /// Selects path labels, map keys or side effects (one label: the value, several: a map; an
    /// undeclared label is an error, see the book page Repeated Labels: select with Pop). Rhai
    /// binding of `select()`.
    pub fn select_single(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        if arg.is::<ScriptGraphTraversalSource>() {
            // `select(traversal)`: TinkerPop's `TraversalSelectStep` with `Pop.last`.
            return Ok(self.add_step(Step::SelectTraversal(Pop::Last, try_dynamic_to_gts(arg)?)));
        }
        if let Some(key) = select_token(&arg)? {
            return Ok(self.add_step(Step::SelectToken(key)));
        }
        self.select(vec![arg])
    }

    /// `select(labels...)` and `select(Pop.x, labels...)`.
    ///
    /// A [`Pop`] first argument selects the pop form: the rest are the labels (strings, or one
    /// array of strings). The `Pop` is checked before any argument is stringified, so
    /// `select(Pop.first, "a")` can never silently read the labels `first` and `a`. A lone `Pop`,
    /// a `Pop` anywhere but first, and a non-string label after a `Pop` are an `ArgumentMismatch`.
    /// Without a `Pop` the labels are read exactly as before.
    pub fn select(&mut self, arg: Vec<Dynamic>) -> Result<Self, Box<rhai::EvalAltResult>> {
        let mut args = arg.into_iter();
        let first = args.next();
        let pop = first
            .as_ref()
            .and_then(|a| a.read_lock::<Pop>().map(|p| *p));
        let Some(pop) = pop else {
            let all: Vec<Dynamic> = first.into_iter().chain(args).collect();
            if let Some(token) = all
                .iter()
                .find(|a| select_token(a).ok().flatten().is_some())
            {
                return Err(select_token_mismatch(&token_text(token)));
            }
            if let Some(position) = all.iter().position(|a| a.is::<Pop>()) {
                return Err(select_pop_mismatch(format!(
                    "a {} in position {} (Pop must be the first argument)",
                    dynamic_text(&all[position]),
                    position + 1
                )));
            }
            return Ok(self.add_step(Step::Select(vec_dynamic_to_compact_string("select", all)?)));
        };
        let rest: Vec<Dynamic> = args.collect();
        if rest.is_empty() {
            return Err(select_pop_mismatch(format!("{pop} without a label")));
        }
        if rest
            .iter()
            .any(|item| item.is::<ScriptGraphTraversalSource>())
        {
            // `select(Pop, traversal)`: exactly one key traversal, TinkerPop has no overload that
            // mixes a traversal with other keys.
            let mut rest = rest;
            if rest.len() != 1 {
                return Err(select_pop_mismatch(format!(
                    "{pop} with {} keys, one of them a traversal (a traversal key is the only key)",
                    rest.len()
                )));
            }
            let key = rest.remove(0);
            return Ok(self.add_step(Step::SelectTraversal(pop, try_dynamic_to_gts(key)?)));
        }
        let mut labels: Vec<CompactString> = Vec::with_capacity(rest.len());
        for item in rest {
            let single = item
                .read_lock::<rhai::ImmutableString>()
                .map(|l| CompactString::new(l.as_str()));
            if let Some(label) = single {
                labels.push(label);
            } else if item.is::<rhai::Array>() {
                for inner in item.cast::<rhai::Array>() {
                    let label = inner
                        .read_lock::<rhai::ImmutableString>()
                        .map(|l| CompactString::new(l.as_str()));
                    match label {
                        Some(label) => labels.push(label),
                        None => {
                            return Err(select_pop_mismatch(format!(
                                "a list holding a value of type {} (`{}`)",
                                type_name_of(&inner),
                                dynamic_text(&inner)
                            )));
                        }
                    }
                }
            } else {
                return Err(select_pop_mismatch(format!(
                    "a value of type {} (`{}`) after {pop}",
                    type_name_of(&item),
                    dynamic_text(&item)
                )));
            }
        }
        Ok(self.add_step(Step::SelectPop(pop, labels)))
    }

    /// `select(Column.keys)` / `select(Column.values)`: the keys/values of a map, or the
    /// key/value of an entry produced by `unfold()`; see `GraphTraversalSource::select`.
    pub fn select_column(&mut self, column: Column) -> Self {
        self.add_step(Step::SelectColumn(column))
    }

    /// Yields the path of each traverser (every element it visited), modulated by by() round-robin.
    /// Rhai binding of `path()`.
    pub fn path(&mut self) -> Self {
        self.add_step(Step::Path)
    }

    /// `repeat(traversal)`: loops the traversal; see `GraphTraversalSource::repeat`.
    pub fn repeat(&mut self, body: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let gts = try_dynamic_to_gts(body)?;
        Ok(self.add_step(Step::Repeat(None, gts)))
    }

    /// `repeat(name, traversal)`: a loop that `loops(name)` reads.
    pub fn repeat_named(
        &mut self,
        name: String,
        body: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let gts = try_dynamic_to_gts(body)?;
        Ok(self.add_step(Step::Repeat(Some(CompactString::new(name)), gts)))
    }

    /// `times(n)`: stops a `repeat()` loop after `n` iterations.
    pub fn times(&mut self, n: i64) -> Result<Self, Box<rhai::EvalAltResult>> {
        let n = u32::try_from(n).map_err(|_| {
            Box::new(rhai::EvalAltResult::ErrorRuntime(
                Dynamic::from(format!(
                    "times() expects a number of iterations from 0 to {}, got {n}",
                    u32::MAX
                )),
                rhai::Position::NONE,
            ))
        })?;
        Ok(self.add_step(Step::Times(n)))
    }

    /// `until(traversal)`: stops a `repeat()` loop for a traverser once the traversal matches.
    pub fn until(&mut self, condition: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let gts = try_dynamic_to_gts(condition)?;
        Ok(self.add_step(Step::Until(gts)))
    }

    /// `emit()`: also outputs every traverser a `repeat()` loop visits.
    pub fn emit_empty(&mut self) -> Self {
        self.add_step(Step::Emit(None))
    }

    /// `emit(traversal)`: also outputs the visited traversers the traversal matches.
    pub fn emit(&mut self, condition: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let gts = try_dynamic_to_gts(condition)?;
        Ok(self.add_step(Step::Emit(Some(gts))))
    }

    /// `loops()`: the iteration count of the innermost enclosing `repeat()`.
    pub fn loops_empty(&mut self) -> Self {
        self.add_step(Step::Loops(None))
    }

    /// `loops(name)`: the iteration count of the enclosing loop named `name`.
    pub fn loops(&mut self, name: String) -> Self {
        self.add_step(Step::Loops(Some(CompactString::new(name))))
    }

    /// `choose(predicate, onTrue, onFalse)`: the 3-argument if/then/else form. See
    /// `GraphTraversalSource::choose`.
    pub fn choose3(
        &mut self,
        predicate: Dynamic,
        on_true: Dynamic,
        on_false: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let predicate = predicate_to_gts("choose", predicate)?;
        let on_true = try_dynamic_to_gts(on_true)?;
        let on_false = try_dynamic_to_gts(on_false)?;
        Ok(self.add_step(Step::Choose3(predicate, on_true, on_false)))
    }

    /// `choose(predicate, onTrue)`: the two-argument if/then form; a traverser that fails the
    /// condition passes through unchanged (the false branch is `__.identity()`, as in TinkerPop).
    pub fn choose2(
        &mut self,
        predicate: Dynamic,
        on_true: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let predicate = predicate_to_gts("choose", predicate)?;
        let on_true = try_dynamic_to_gts(on_true)?;
        Ok(self.add_step(Step::Choose3(
            predicate,
            on_true,
            one_step_source(Step::Identity),
        )))
    }

    /// `choose(traversal)`: the chained switch form, extended with `.option(pick, traversal)`.
    /// See `GraphTraversalSource::choose_switch`.
    pub fn choose_switch(&mut self, selector: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let selector = selector_to_gts("choose", selector)?;
        Ok(self.add_step(Step::ChooseSwitch(selector)))
    }

    /// `branch(traversal)`: the fan-out counterpart of `choose(traversal)`. See
    /// `GraphTraversalSource::branch`.
    pub fn branch(&mut self, selector: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        let selector = selector_to_gts("branch", selector)?;
        Ok(self.add_step(Step::Branch(selector)))
    }

    /// `.option(pick, traversal)`: chains onto the preceding `choose(traversal)`/
    /// `branch(traversal)`. `pick` is a `Pick.any`/`Pick.none`/`Pick.unproductive` sentinel, a
    /// predicate (`P.between(26, 30)`), a traversal (`branch()` only) or a plain scalar value
    /// compared like `P.eq` against the selector's produced value.
    pub fn option(
        ctx: NativeCallContext,
        this: &mut Self,
        pick: Dynamic,
        branch: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let max_depth = access::value_depth_of(&ctx);
        // `option(Merge.onCreate|onMatch|outV|inV, map|()|traversal)` belongs to a merge step.
        if let Some(token) = pick.read_lock::<Merge>().map(|token| *token) {
            let branch = script_merge_arg(branch, max_depth)?;
            return Ok(this.add_step(Step::MergeOption(token, branch, None)));
        }
        let branch = try_dynamic_to_gts(branch)?;
        if pick.type_name() == std::any::type_name::<ScriptGraphTraversalSource>() {
            let key = try_dynamic_to_gts(pick)?;
            return Ok(this.add_step(Step::OptionTraversal(key, branch)));
        }
        let token = if let Some(predicate) = pick.clone().read_lock::<ArgP>() {
            validate_predicate_operands(&predicate)?;
            OptionToken::Predicate(argp_to_p(&predicate))
        } else {
            match pick.clone().try_cast::<Pick>() {
                Some(Pick::Any) => OptionToken::Any,
                Some(Pick::None) => OptionToken::None,
                Some(Pick::Unproductive) => OptionToken::Unproductive,
                None => OptionToken::Value(dynamic_to_element_property(pick, max_depth)?),
            }
        };
        Ok(this.add_step(Step::Option(token, branch)))
    }

    /// `simple_path()`/`simplePath()`: keeps traversers whose path visits no object twice.
    pub fn simple_path(&mut self) -> Self {
        self.add_step(Step::SimplePath)
    }

    /// `cyclic_path()`/`cyclicPath()`: keeps traversers whose path visits some object twice.
    pub fn cyclic_path(&mut self) -> Self {
        self.add_step(Step::CyclicPath)
    }

    // pub fn where_(&mut self, arg: ScriptGraphTraversalSource) -> Self {
    //     self.add_step(Step::Where(arg))
    // }
}

/// The token key of `select(T.id)`, `select(T.label)`, `select(Direction.OUT)` or
/// `select(Direction.IN)`; `None` for any other argument. `T.key`, `T.value` and
/// `Direction.BOTH` name no map entry and are an argument error.
fn select_token(
    arg: &Dynamic,
) -> Result<Option<crate::value::RecordKey>, Box<rhai::EvalAltResult>> {
    use crate::value::RecordKey;
    if let Some(by) = arg.read_lock::<ArgBy>() {
        return match &*by {
            ArgBy::Id => Ok(Some(RecordKey::Id)),
            ArgBy::Label => Ok(Some(RecordKey::Label)),
            _ => Err(select_token_mismatch(&token_text(arg))),
        };
    }
    if let Some(direction) = arg.read_lock::<Direction>() {
        return match *direction {
            Direction::Out => Ok(Some(RecordKey::Out)),
            Direction::In => Ok(Some(RecordKey::In)),
            Direction::Both => Err(select_token_mismatch(&token_text(arg))),
        };
    }
    Ok(None)
}

/// The argument error of a token `select()` cannot read.
fn select_token_mismatch(got: &str) -> Box<rhai::EvalAltResult> {
    script_error_from_script(crate::error::ScriptError::ArgumentMismatch {
        step_name: "select".to_string(),
        expected: "select(T.id), select(T.label), select(Direction.OUT) or select(Direction.IN): \
                   one token alone, reading that entry of an elementMap()/valueMap(true) map"
            .to_string(),
        actual: got.to_string(),
    })
}

/// A token argument as its DSL literal (`T.key`, `Direction.BOTH`).
fn token_text(arg: &Dynamic) -> String {
    crate::script::render::token_literal(arg).unwrap_or_else(|| dynamic_text(arg))
}
