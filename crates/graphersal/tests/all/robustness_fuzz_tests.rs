//! Randomized robustness harness for the Rhai DSL (CLAUDE.md rule 1: arbitrary user queries must
//! never crash the engine).
//!
//! A seeded generator builds random traversal scripts from the registered step surface (the
//! `## signatures` section of `tests/snapshots/script_surface.txt`: every step name with every
//! arity it is registered with) and random arguments (strings, extreme integers, NaN/inf, empty and
//! huge lists, nested maps, `P`/`TextP` predicates, tokens, `jpath`, `UUID`, nested `__.`
//! traversals, `repeat` with and without `times`, mutating steps). Each script runs on a fresh
//! modern graph and on an empty graph, under the SAFE [`ScriptLimits`], a host
//! `evaluationTimeout` and everything allowed except files outside a fresh temporary directory
//! (the case's file root: the generated `export_graphml("marko")` and friends write there and
//! never into the working directory; the directory is removed after the case), in its own thread
//! under `catch_unwind` with a wall-clock watchdog.
//!
//! Checked per case:
//! - **panic**: `catch_unwind` caught one (always a failure);
//! - **hang**: the watchdog fired (the script ran far past its `evaluationTimeout`);
//! - **unlocated**: a step failed at run time (`... execution failed`) but the diagnostic carries
//!   no `at #N:` location;
//! - **no-location**: an error carries neither a step location (`at #N`) nor a script position
//!   (`(line N, position M)`), unless it is one of the errors that have no step
//!   (`NO_STEP_ERRORS`: options and denied `with()` options, which name their key);
//! - **debug-leak**: the diagnostic contains Rust `Debug` text (`Some(`, `String("`, ...);
//! - **type-leak**: the diagnostic names a Rust type (`graphersal::...::ArgP`, `f64`) instead of
//!   its display name (`Predicate`, `float64`);
//! - **timeout-overshoot**: `evaluationTimeout` fired far too late.
//!
//! The fast budget and the edge probes run in every `cargo test` (fixed seed, a few hundred
//! cases); they fail on every finding but a timeout overshoot (timing-dependent in a debug
//! build), unless it is a known, documented panic (`KNOWN_PANICS`; each has an ignored `BUG:`
//! regression test in `robustness_bug_tests.rs`). The long budget is opt-in:
//!
//! ```text
//! ROBUSTNESS_CASES=20000 ROBUSTNESS_SEED=1 ROBUSTNESS_THREADS=8 \
//!   cargo test --release --package graphersal --features script,io,display,serde \
//!   --test all robustness_fuzz_tests::fuzz_long -- --ignored --nocapture
//! ```
//!
//! It writes every distinct finding, minimized, to `target/robustness-findings.txt`
//! (`ROBUSTNESS_OUT` overrides the path). A single case replays with `ROBUSTNESS_REPLAY=<seed>`.
#![cfg(feature = "script")]

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use graphersal::exec::RenderOptions;
use graphersal::prelude::*;
use graphersal::script::rhai::Dynamic;

use crate::script_host::ScriptLimits;

// ---------------------------------------------------------------------------------------------
// Deterministic RNG (SplitMix64): tiny, dependency-free and stable across toolchains, so a seed
// reproduces the same script forever.
// ---------------------------------------------------------------------------------------------

/// SplitMix64 generator.
#[derive(Clone)]
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub(crate) fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
    pub(crate) fn chance(&mut self, percent: u64) -> bool {
        self.next_u64() % 100 < percent
    }
    pub(crate) fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

// ---------------------------------------------------------------------------------------------
// The step surface.
// ---------------------------------------------------------------------------------------------

const SURFACE: &str = include_str!("../snapshots/script_surface.txt");

/// Steps the harness never calls: they read or write files, or build a new graph source.
const EXCLUDED_STEPS: &[&str] = &[
    "export_graphml",
    "import_graphml",
    "export_graphson",
    "exportGraphson",
    "import_graphson",
    "importGraphson",
    "export_snapshot",
    "exportSnapshot",
    "loadSchema",
    "load_schema",
    "file",
    "file_tree",
    "tinkerpop_modern",
    "empty",
];

/// Steps that end a traversal (they return data, a rendering or a report, not a traversal): the
/// generator only puts them last.
const TERMINAL_STEPS: &[&str] = &[
    "next",
    "iterate",
    "toList",
    "to_list",
    "execute",
    "profile",
    "profile_with",
    "visualize",
    "statistics",
    "mark",
    "memoryUsage",
    "memory_usage",
    "toGraph",
    "to_graph",
    "toJson",
    "to_json",
    "toJsonSchema",
    "to_json_schema",
    "toMarkdown",
    "to_markdown",
    "toMermaid",
    "to_mermaid",
    "toPlantUML",
    "toPlantuml",
    "to_plantuml",
    "toTable",
    "to_table",
    "toTree",
    "to_tree",
    "toMap",
    "to_map",
    "schema",
    "setVisualizer",
    "set_visualizer",
];

/// The chainable steps of the surface, with their arities.
fn step_table() -> Vec<(String, Vec<usize>)> {
    surface_steps()
        .into_iter()
        .filter(|(n, _)| !TERMINAL_STEPS.contains(&n.as_str()))
        .collect()
}

/// The terminal steps of the surface, with their arities.
fn terminal_table() -> Vec<(String, Vec<usize>)> {
    surface_steps()
        .into_iter()
        .filter(|(n, _)| TERMINAL_STEPS.contains(&n.as_str()))
        .collect()
}

/// Every traversal step name with the arities it is registered with, parsed from the
/// `## signatures` section of the surface snapshot (first parameter `&mut GraphTraversalSource`).
fn surface_steps() -> Vec<(String, Vec<usize>)> {
    let mut table: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut in_signatures = false;
    for line in SURFACE.lines() {
        if line.starts_with("## ") {
            in_signatures = line.starts_with("## signatures");
            continue;
        }
        if !in_signatures {
            continue;
        }
        let Some((name, rest)) = line.split_once("(_: &mut GraphTraversalSource") else {
            continue;
        };
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        if EXCLUDED_STEPS.contains(&name) {
            continue;
        }
        let params = rest.split(") ->").next().unwrap_or("");
        let arity = params.matches(", _:").count();
        let entry = table.entry(name.to_string()).or_default();
        if !entry.contains(&arity) {
            entry.push(arity);
        }
    }
    table.into_iter().collect()
}

// ---------------------------------------------------------------------------------------------
// The generated script, as a small AST so failures can be minimized structurally.
// ---------------------------------------------------------------------------------------------

/// One step call `name(args...)`.
#[derive(Clone, Debug)]
pub(crate) struct Call {
    name: String,
    args: Vec<Arg>,
}

/// One argument of a step call.
#[derive(Clone, Debug)]
pub(crate) enum Arg {
    /// Literal Rhai source (a string, number, token, predicate, map, ...).
    Lit(String),
    /// A nested anonymous traversal `__.a().b()`.
    Anon(Vec<Call>),
}

/// A whole script: `<source>.<steps>.<terminal>`.
#[derive(Clone, Debug)]
pub(crate) struct Script {
    source: String,
    steps: Vec<Call>,
    terminal: String,
}

fn render_calls(out: &mut String, calls: &[Call]) {
    for call in calls {
        let _ = write!(out, ".{}(", call.name);
        for (i, arg) in call.args.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            match arg {
                Arg::Lit(text) => out.push_str(text),
                Arg::Anon(inner) => {
                    out.push_str("__");
                    if inner.is_empty() {
                        out.push_str(".identity()");
                    }
                    render_calls(out, inner);
                }
            }
        }
        out.push(')');
    }
}

impl Script {
    pub(crate) fn render(&self) -> String {
        let mut out = self.source.clone();
        render_calls(&mut out, &self.steps);
        out.push_str(&self.terminal);
        out
    }
}

// ---------------------------------------------------------------------------------------------
// Generator.
// ---------------------------------------------------------------------------------------------

const STRINGS: &[&str] = &[
    r#""name""#,
    r#""age""#,
    r#""lang""#,
    r#""weight""#,
    r#""person""#,
    r#""software""#,
    r#""knows""#,
    r#""created""#,
    r#""marko""#,
    r#""a""#,
    r#""x""#,
    r#""""#,
    r#"" ""#,
    r#""_""#,
    r#""T.id""#,
    r#""T.label""#,
    r#""Direction.OUT""#,
    r#""$.a""#,
    r#""a.b[1]""#,
    r#""\x00""#,
    r#""ü€😀""#,
    r#""%s%n{}""#,
    r#""1""#,
    r#""-1""#,
    r#""NaN""#,
    r#""9223372036854775808""#,
    r#""00000000-0000-0000-0000-000000000000""#,
    r#""*""#,
    r#""**/*""#,
    r#""[""#,
    r#""a + b * _""#,
    r#""_ / 0""#,
    r#""sin(_) ^ 1e308""#,
    "big_string",
];

const INTS: &[&str] = &[
    "0",
    "1",
    "2",
    "3",
    "-1",
    "-2",
    "10",
    "100",
    "1000000",
    "4294967296",
    "imax",
    "imin",
    "(imax - 1)",
];

const FLOATS: &[&str] = &[
    "0.0", "-0.0", "0.5", "1.5", "-2.5", "1e308", "nan", "inf", "ninf",
];

const TOKENS: &[&str] = &[
    "T.id",
    "T.label",
    "T::id",
    "Order.asc",
    "Order.desc",
    "Order::Asc",
    "Scope.local",
    "Scope.global",
    "Column.keys",
    "Column.values",
    "keys",
    "values",
    "Direction.OUT",
    "Direction.IN",
    "Direction.BOTH",
    "Direction.from",
    "Cardinality.single",
    "Cardinality.list",
    "Cardinality.set",
    "Cardinality::list(1)",
    "Pop.first",
    "Pop.last",
    "Pop.all",
    "Pop.mixed",
    "first",
    "last",
    "Operator.sum",
    "Operator.mult",
    "Operator.div",
    "Operator.minus",
    "Operator.addAll",
    "Operator.assign",
    "Operator.and",
    "Operator.max",
    "Operator.sumLong",
    "Barrier.normSack",
    "GType.LONG",
    "GType.INT",
    "GType.DOUBLE",
    "GType.STRING",
    "GType.UUID",
    "GType.VERTEX",
    "GType.MAP",
    "GType.LIST",
    "GType.NULL",
    "GType.NUMBER",
    "Merge.onCreate",
    "Merge.onMatch",
    "Merge.outV",
    "Merge.inV",
    "Pick.any",
    "Pick.none",
    "By.Count",
    "CastPolicy.Skip",
    "CastPolicy.Error",
    "CastPolicy::Default(0)",
    "ProfileType.Memory",
    "V.Table",
    "V.Json",
    "V.Mermaid",
    "V.Tree",
    "Order.shuffle",
    "T.key",
    "T.value",
    "WithOptions.tokens",
    "WithOptions.indexer",
    "WithOptions.list",
    "WithOptions.map",
    "WithOptions.all",
];

const PRED_NAMES_1: &[&str] = &["eq", "neq", "gt", "gte", "lt", "lte"];
const PRED_NAMES_2: &[&str] = &["between", "inside", "outside"];
const TEXT_PRED_NAMES: &[&str] = &[
    "containing",
    "startingWith",
    "endingWith",
    "notContaining",
    "notStartingWith",
    "notEndingWith",
    "regex",
    "notRegex",
];

const JPATHS: &[&str] = &[
    r#""a""#,
    r#""a.b""#,
    r#""a.b[1]""#,
    r#""a[-1]""#,
    r#""$['a']""#,
    r#""a[9223372036854775807]""#,
    r#""a[-9223372036854775808]""#,
    r#""""#,
    r#""[""#,
    r#""a..b""#,
    r#""a[*]""#,
    r#""$""#,
];

/// Steps that make good sources after `g.`.
const SOURCES: &[&str] = &[
    "g.v()",
    "g.V()",
    "g.v(1)",
    "g.v(1, 2, 3)",
    "g.e()",
    "g.E(7)",
    "g.inject(1, 2, 3)",
    "g.inject([1, 2], #{a: 1})",
    "g.inject(nan, inf, imin)",
    "g.addV(\"person\")",
    "g.addV(\"person\").property(\"name\", \"z\")",
    "g.mergeV(#{name: \"marko\"})",
    "g.withSack(0).v()",
    "g.withSack(1.0, Operator.sum).v()",
    "g.withSideEffect(\"a\", []).v()",
    "g.withBulk(false).v()",
    "g.with(\"repeat.order\", \"dfs\").v()",
    "g.with(\"bulk.merge\", false).v()",
    "g.with(\"path.analysis\", false).v()",
    "g.with(\"repeat.max_loops\", 3).v()",
    "g.with(\"traversal.max_traversers\", 10).v()",
    "g.with(\"optimizer.disabled\", [\"count_pushdown\"]).v()",
    "_g.inject(1)",
    "g.with(\"random.seed\", 7).v()",
    "g.with(\"render.spelling\", \"camel\").v()",
    "g.withPath().v()",
    // The host presets reach `__` and the sources a script creates (RB2, finding 5).
    "__.inject(1)",
    "GraphSource::empty().inject(1)",
];

/// Terminals appended to the script; `""` leaves the traversal to the renderer.
const TERMINALS: &[&str] = &[
    ".toList()",
    ".to_list()",
    ".next()",
    "",
    "",
    ".toList().len()",
    ".profile()",
    ".execute()",
    ".to_table()",
    ".to_json()",
    ".count().next()",
];

/// Typed call templates `name:kinds`; each kind letter is one argument (see `Gen::typed_arg`:
/// K key, L label, T traversal, N int, F float, S scalar, P predicate, p predicate-or-scalar,
/// M math, A step label, G GType, O Order, B by-argument, C Cardinality, X Operator, m map,
/// l list, s Scope, R glob, J JSONPath text, E option key, D Direction, W Pop, w WithOptions,
/// ? anything).
const TYPED: &[&str] = &[
    "out:",
    "out:L",
    "in_:",
    "in_:L",
    "both:",
    "both:L",
    "outE:",
    "inE:L",
    "bothE:",
    "outV:",
    "inV:",
    "otherV:",
    "bothV:",
    "values:",
    "values:K",
    "values:KK",
    "properties:K",
    "properties:",
    "valueMap:",
    "valueMap:K",
    "elementMap:",
    "element_map:K",
    "has:K",
    "has:Kp",
    "has:LKp",
    "hasLabel:L",
    "hasLabel:LL",
    "hasId:N",
    "hasId:S",
    "hasKey:K",
    "hasNot:K",
    "hasValue:S",
    "is:p",
    "where:T",
    "where:P",
    "where:AP",
    "filter:T",
    "not:T",
    "and:TT",
    "or:TT",
    "union:TT",
    "union:T",
    "coalesce:TT",
    "local:T",
    "optional:T",
    "repeat:T",
    "repeat:AT",
    "times:N",
    "until:T",
    "until:P",
    "emit:",
    "emit:T",
    "branch:T",
    "choose:T",
    "choose:TTT",
    "choose:PTT",
    "option:ET",
    "option:Em",
    "sideEffect:T",
    "limit:N",
    "limit:sN",
    "skip:N",
    "tail:N",
    "tail:sN",
    "range:NN",
    "range:sNN",
    "coin:F",
    "count:",
    "count:s",
    "sum:",
    "sum:s",
    "mean:",
    "mean:s",
    "max:",
    "max:s",
    "min:",
    "min:s",
    "median:",
    "fold:",
    "fold:SX",
    "unfold:",
    "dedup:",
    "dedup:A",
    "dedup:s",
    "order:",
    "order:s",
    "by:B",
    "by:BO",
    "group:",
    "groupCount:",
    "project:AA",
    "select:A",
    "select:AA",
    "select:WA",
    "select:T",
    "as_:A",
    "path:",
    "simplePath:",
    "cyclicPath:",
    "tree:",
    "id:",
    "label:",
    "labels:",
    "key:",
    "value:",
    "constant:S",
    "constant:l",
    "constant:m",
    "inject:S",
    "inject:SS",
    "math:M",
    "property:KS",
    "property:CKS",
    "property:KT",
    "removeProperty:K",
    "drop:",
    "addV:L",
    "addV:",
    "addE:L",
    "from:A",
    "to:A",
    "from:T",
    "to:T",
    "mergeV:m",
    "mergeV:",
    "mergeE:m",
    "sack:",
    "sack:X",
    "aggregate:A",
    "store:A",
    "cap:A",
    "cap:AA",
    "loops:",
    "loops:A",
    "barrier:",
    "barrier:N",
    "identity:",
    "none:P",
    "discard:",
    "combine:l",
    "combine:T",
    "difference:l",
    "disjunct:l",
    "intersect:l",
    "intersect:T",
    "merge:l",
    "merge:m",
    "product:l",
    "conjoin:S",
    "concat:SS",
    "concat:TT",
    "format:S",
    "split:S",
    "split:sS",
    "index:",
    "reverse:",
    "sample:N",
    "sample:sN",
    "fail:",
    "fail:S",
    "map:T",
    "flatMap:T",
    "group:A",
    "groupCount:A",
    "dedup:AA",
    "property:TS",
    "property:TT",
    "addV:T",
    "by:",
    "tail:",
    "union:",
    "with:w",
    "with:ww",
    "as_number:",
    "as_number:G",
    "asString:",
    "as_bool:",
    "cast:G",
    "length:",
    "toUpper:",
    "toLower:",
    "trim:",
    "lTrim:",
    "substring:N",
    "substring:NN",
    "replace:SS",
    "globPath:R",
    "json_path:J",
    "propertyJson:K",
    "subgraph:A",
    "element:",
    "toE:D",
    "toE:DL",
    "toV:D",
    "with_side_effect:AS",
    "withBulk:S",
];

struct Gen<'a> {
    rng: Rng,
    table: &'a [(String, Vec<usize>)],
    terminals: &'a [(String, Vec<usize>)],
}

impl Gen<'_> {
    fn call(&mut self, depth: usize) -> Call {
        // A few hand-picked shapes that random choice rarely hits.
        if self.rng.chance(8) {
            return self.special(depth);
        }
        // Mostly well-typed calls, so scripts get past argument validation into execution.
        if self.rng.chance(60) {
            return self.typed(depth);
        }
        let (name, arities) = self.rng.pick(self.table).clone();
        // Prefer small arities, but keep the large ones reachable.
        let arity = if self.rng.chance(85) {
            *arities.iter().min().unwrap_or(&0) + self.rng.below(2)
        } else {
            *self.rng.pick(&arities)
        };
        let arity = if arities.contains(&arity) {
            arity
        } else {
            *self.rng.pick(&arities)
        };
        let args = (0..arity).map(|_| self.arg(depth)).collect();
        Call { name, args }
    }

    /// A call built from a typed template (`TYPED`): each argument letter names a kind.
    fn typed(&mut self, depth: usize) -> Call {
        let template = *self.rng.pick(TYPED);
        let (name, kinds) = template.split_once(':').unwrap_or((template, ""));
        let args = kinds
            .chars()
            .map(|kind| self.typed_arg(kind, depth))
            .collect();
        Call {
            name: name.to_string(),
            args,
        }
    }

    fn typed_arg(&mut self, kind: char, depth: usize) -> Arg {
        let lit = |s: &str| Arg::Lit(s.to_string());
        match kind {
            'K' => {
                if self.rng.chance(15) {
                    Arg::Lit(format!("jpath({})", self.rng.pick(JPATHS)))
                } else {
                    lit(self.rng.pick(&[
                        "\"name\"",
                        "\"age\"",
                        "\"lang\"",
                        "\"weight\"",
                        "\"x\"",
                        "\"\"",
                    ]))
                }
            }
            'L' => lit(self.rng.pick(&[
                "\"person\"",
                "\"software\"",
                "\"knows\"",
                "\"created\"",
                "\"x\"",
                "\"\"",
            ])),
            'T' if depth < 3 => {
                let len = 1 + self.rng.below(3);
                Arg::Anon(self.chain(depth + 1, len))
            }
            'T' => Arg::Anon(vec![Call {
                name: "identity".into(),
                args: vec![],
            }]),
            'N' => {
                lit(self
                    .rng
                    .pick(&["0", "1", "2", "3", "-1", "10", "imax", "imin", "4294967296"]))
            }
            'F' => lit(self.rng.pick(FLOATS)),
            'S' => Arg::Lit(self.scalar()),
            'P' => Arg::Lit(self.predicate()),
            'p' => {
                if self.rng.chance(50) {
                    Arg::Lit(self.predicate())
                } else {
                    Arg::Lit(self.scalar())
                }
            }
            'M' => lit(self.rng.pick(&[
                "\"_ + 1\"",
                "\"_ / 0\"",
                "\"_ % 0\"",
                "\"a - b\"",
                "\"sin(_)\"",
                "\"_ ^ 1e308\"",
                "\"sqrt(-1)\"",
                "\"abs(_) * -_\"",
                "\"((((((((((_))))))))))\"",
                "\"_ +\"",
                "\"\"",
                "\"x\"",
                "\"ceil(_) / floor(_)\"",
            ])),
            'A' => lit(self.rng.pick(&["\"a\"", "\"b\"", "\"c\""])),
            'G' => lit(self.rng.pick(&[
                "GType.LONG",
                "GType.INT",
                "GType.DOUBLE",
                "GType.STRING",
                "GType.UUID",
                "GType.BOOLEAN",
                "GType.NULL",
                "GType.LIST",
                "GType.MAP",
                "GType.NUMBER",
                "GType.FLOAT",
            ])),
            'O' => lit(self.rng.pick(&["Order.asc", "Order.desc", "Order.shuffle"])),
            'B' => match self.rng.below(5) {
                0 => self.typed_arg('K', depth),
                1 => self.typed_arg('T', depth),
                2 => lit(self
                    .rng
                    .pick(&["T.id", "T.label", "T.key", "T.value", "By.Count"])),
                3 => self.typed_arg('O', depth),
                _ => lit(self.rng.pick(&["Column.keys", "Column.values"])),
            },
            'C' => {
                lit(self
                    .rng
                    .pick(&["Cardinality.single", "Cardinality.list", "Cardinality.set"]))
            }
            'X' => lit(self.rng.pick(&[
                "Operator.sum",
                "Operator.minus",
                "Operator.mult",
                "Operator.div",
                "Operator.min",
                "Operator.max",
                "Operator.and",
                "Operator.or",
                "Operator.addAll",
                "Operator.assign",
                "Operator.sumLong",
            ])),
            'm' => Arg::Lit(self.map(depth)),
            'l' => Arg::Lit(self.list(depth)),
            's' => lit(self.rng.pick(&["Scope.local", "Scope.global"])),
            'R' => lit(self.rng.pick(&[
                "\"*\"",
                "\"**\"",
                "\"**/**/**/**\"",
                "\"a/*/b\"",
                "\"[\"",
                "\"{a,b}\"",
                "\"\"",
            ])),
            'J' => lit(self.rng.pick(&[
                "\"$.a\"",
                "\"$..a\"",
                "\"$[*]\"",
                "\"$\"",
                "\"[\"",
                "\"$.a[-1]\"",
            ])),
            'E' => lit(self.rng.pick(&[
                "Merge.onCreate",
                "Merge.onMatch",
                "Pick.any",
                "Pick.none",
                "1",
                "\"marko\"",
            ])),
            'D' => lit(self
                .rng
                .pick(&["Direction.OUT", "Direction.IN", "Direction.BOTH"])),
            'W' => lit(self
                .rng
                .pick(&["Pop.first", "Pop.last", "Pop.all", "Pop.mixed"])),
            'w' => lit(self.rng.pick(&[
                "WithOptions.tokens",
                "WithOptions.none",
                "WithOptions.ids",
                "WithOptions.labels",
                "WithOptions.keys",
                "WithOptions.values",
                "WithOptions.all",
                "WithOptions.indexer",
                "WithOptions.list",
                "WithOptions.map",
            ])),
            _ => self.arg(depth),
        }
    }

    fn special(&mut self, depth: usize) -> Call {
        let anon = |g: &mut Self| {
            let len = 1 + g.rng.below(2);
            Arg::Anon(g.chain(depth + 1, len))
        };
        match self.rng.below(10) {
            0 => Call {
                name: "repeat".into(),
                args: vec![anon(self)],
            },
            1 => Call {
                name: "times".into(),
                args: vec![Arg::Lit(self.rng.pick(INTS).to_string())],
            },
            2 => Call {
                name: "until".into(),
                args: vec![anon(self)],
            },
            3 => Call {
                name: "emit".into(),
                args: vec![],
            },
            4 => Call {
                name: "by".into(),
                args: vec![self.arg(depth)],
            },
            5 => Call {
                name: "as_".into(),
                args: vec![Arg::Lit(r#""a""#.into())],
            },
            6 => Call {
                name: "select".into(),
                args: vec![Arg::Lit(r#""a""#.into()), Arg::Lit(r#""b""#.into())],
            },
            7 => Call {
                name: "property".into(),
                args: vec![self.arg(depth), self.arg(depth)],
            },
            8 => Call {
                name: "option".into(),
                args: vec![self.arg(depth), anon(self)],
            },
            _ => Call {
                name: "drop".into(),
                args: vec![],
            },
        }
    }

    fn chain(&mut self, depth: usize, len: usize) -> Vec<Call> {
        (0..len).map(|_| self.call(depth)).collect()
    }

    fn scalar(&mut self) -> String {
        match self.rng.below(6) {
            0 | 1 => self.rng.pick(STRINGS).to_string(),
            2 | 3 => self.rng.pick(INTS).to_string(),
            4 => self.rng.pick(FLOATS).to_string(),
            _ => self.rng.pick(&["true", "false", "()"]).to_string(),
        }
    }

    fn arg(&mut self, depth: usize) -> Arg {
        let nested_ok = depth < 3;
        match self.rng.below(20) {
            0..=4 => Arg::Lit(self.rng.pick(STRINGS).to_string()),
            5..=7 => Arg::Lit(self.rng.pick(INTS).to_string()),
            8 => Arg::Lit(self.rng.pick(FLOATS).to_string()),
            9 => Arg::Lit(self.rng.pick(TOKENS).to_string()),
            10 => Arg::Lit(self.predicate()),
            11 => Arg::Lit(self.list(depth)),
            12 => Arg::Lit(self.map(depth)),
            13 => Arg::Lit(format!("jpath({})", self.rng.pick(JPATHS))),
            14 => Arg::Lit(
                self.rng
                    .pick(&[
                        r#"UUID.fromString("00000000-0000-0000-0000-000000000001")"#,
                        r#"UUID.fromString("ffffffff-ffff-ffff-ffff-ffffffffffff")"#,
                        "big_list",
                        "[]",
                        "#{}",
                        "()",
                        "true",
                    ])
                    .to_string(),
            ),
            _ if nested_ok => {
                let len = self.rng.below(3);
                Arg::Anon(self.chain(depth + 1, len))
            }
            _ => Arg::Lit(self.scalar()),
        }
    }

    fn predicate(&mut self) -> String {
        let base = match self.rng.below(7) {
            0 | 1 => format!("P.{}({})", self.rng.pick(PRED_NAMES_1), self.scalar()),
            2 => format!(
                "P.{}({}, {})",
                self.rng.pick(PRED_NAMES_2),
                self.scalar(),
                self.scalar()
            ),
            3 => {
                let n = self.rng.below(4);
                let items: Vec<String> = (0..n).map(|_| self.scalar()).collect();
                let name = self.rng.pick(&["within", "without"]);
                if self.rng.chance(50) {
                    format!("P.{name}([{}])", items.join(", "))
                } else {
                    format!("P.{name}({})", items.join(", "))
                }
            }
            4 => format!(
                "TextP.{}({})",
                self.rng.pick(TEXT_PRED_NAMES),
                self.scalar()
            ),
            5 => format!(
                "P.typeOf({})",
                self.rng.pick(&["GType.LONG", "GType.STRING", "GType.NULL"])
            ),
            _ => format!(
                "P.not(P.{}({}))",
                self.rng.pick(PRED_NAMES_1),
                self.scalar()
            ),
        };
        if self.rng.chance(15) {
            format!("{base}.negate()")
        } else {
            base
        }
    }

    fn list(&mut self, depth: usize) -> String {
        let n = self.rng.below(4);
        let items: Vec<String> = (0..n)
            .map(|_| {
                if depth < 3 && self.rng.chance(20) {
                    self.list(depth + 1)
                } else {
                    self.scalar()
                }
            })
            .collect();
        format!("[{}]", items.join(", "))
    }

    fn map(&mut self, depth: usize) -> String {
        let n = self.rng.below(4);
        let keys = ["name", "age", "a", "b", "`T.id`", "`T.label`"];
        let start = self.rng.below(keys.len());
        let items: Vec<String> = (0..n)
            .map(|i| {
                let key = keys[(start + i) % keys.len()].replace('`', "\"");
                let value = if depth < 3 && self.rng.chance(25) {
                    self.map(depth + 1)
                } else if self.rng.chance(15) {
                    self.rng.pick(TOKENS).to_string()
                } else {
                    self.scalar()
                };
                format!("{key}: {value}")
            })
            .collect();
        format!("#{{{}}}", items.join(", "))
    }

    fn script(&mut self) -> Script {
        let source = self.rng.pick(SOURCES).to_string();
        let len = self.rng.below(6);
        let steps = self.chain(0, len);
        let terminal = if self.rng.chance(30) {
            // A random terminal step with random arguments.
            let (name, arities) = self.rng.pick(self.terminals).clone();
            let arity = *self.rng.pick(&arities);
            let call = Call {
                name,
                args: (0..arity).map(|_| self.arg(1)).collect(),
            };
            let mut text = String::new();
            render_calls(&mut text, &[call]);
            text
        } else {
            self.rng.pick(TERMINALS).to_string()
        };
        Script {
            source,
            steps,
            terminal,
        }
    }
}

/// The script generated for `seed` (deterministic).
pub(crate) fn generate(seed: u64) -> Script {
    let table = step_table();
    let terminals = terminal_table();
    let mut generator = Gen {
        rng: Rng::new(seed),
        table: &table,
        terminals: &terminals,
    };
    generator.script()
}

// ---------------------------------------------------------------------------------------------
// Running one case.
// ---------------------------------------------------------------------------------------------

/// Which graph a case runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GraphKind {
    Modern,
    Empty,
}

/// Classification of one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Ok,
    /// A clean error; the text is the diagnostic.
    Error(String),
    Panic(String),
    Hang,
}

/// Host `evaluationTimeout` of every case, in milliseconds.
const EVALUATION_TIMEOUT_MS: i64 = 1_000;

/// Host `memory.limit` of every case, in bytes (like the openCypher TCK runner): an unbounded
/// `repeat()` stops with the memory limit error instead of growing to gigabytes within the
/// `evaluationTimeout`. Without a tracking allocator in this binary the engine's estimate is
/// compared.
///
/// The value is 128 MiB rather than 512 MiB to compensate for that estimate undercounting
/// unbounded loops several times over (512 MiB let one case reach about 3 GB of resident
/// memory); it returns to 512 MiB once the memory estimate counts traverser values.
const MEMORY_LIMIT_BYTES: i64 = 128 * 1024 * 1024;

/// A fresh, empty directory for one case: its file root. Removed by `Drop`.
struct CaseDir(std::path::PathBuf);

impl CaseDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "graphersal-fuzz-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("create the case's temporary directory");
        CaseDir(dir)
    }
}

impl Drop for CaseDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_inner(script: &str, kind: GraphKind) -> Outcome {
    run_inner_with(script, kind, EVALUATION_TIMEOUT_MS, MEMORY_LIMIT_BYTES)
}

/// [`run_inner`] with an `evaluationTimeout` of `timeout_ms` and a `memory.limit` of
/// `memory_limit` bytes.
fn run_inner_with(script: &str, kind: GraphKind, timeout_ms: i64, memory_limit: i64) -> Outcome {
    // Files only inside the case's own temporary directory: a path that leaves it (`..`, an
    // absolute path, a symbolic link) is denied, so a generated export never touches real files.
    let case_dir = CaseDir::new();
    let authorizer: Arc<dyn graphersal::auth::Authorizer> =
        Arc::new(graphersal::auth::AccessPolicy::allow_all().with_file_root(&case_dir.0));
    let graph = Arc::new(match kind {
        GraphKind::Modern => GraphSource::tinkerpop_modern(),
        GraphKind::Empty => GraphSource::empty(),
    });
    let limits = ScriptLimits::default();
    let presets = [
        ("evaluationTimeout", ElementProperty::from(timeout_ms)),
        ("memory.limit", ElementProperty::from(memory_limit)),
    ];
    let engine = graphersal::script::engine_with_options(&limits, authorizer.clone(), &presets);
    let mut scope = graphersal::script::graph_scope_with_options(graph, &presets, authorizer);
    scope.push("nan", f64::NAN);
    scope.push("inf", f64::INFINITY);
    scope.push("ninf", f64::NEG_INFINITY);
    scope.push("imax", i64::MAX);
    scope.push("imin", i64::MIN);
    scope.push("big_string", "x".repeat(100_000));
    let big: graphersal::script::rhai::Array = (0..10_000_i64).map(Dynamic::from).collect();
    scope.push("big_list", big);
    match engine.eval_with_scope::<Dynamic>(&mut scope, script) {
        Ok(value) => match graphersal::script::try_render_value(
            value,
            VisualizeFormat::Auto,
            &RenderOptions::new(),
        ) {
            Ok(_) => Outcome::Ok,
            Err(text) => Outcome::Error(text),
        },
        Err(err) => Outcome::Error(graphersal::script::map_eval_error(err, &limits).to_string()),
    }
}

/// Runs `script` on a fresh graph of `kind` in its own thread, under `catch_unwind` and a
/// watchdog of `watchdog`.
pub(crate) fn run_case(script: &str, kind: GraphKind, watchdog: Duration) -> Outcome {
    let owned = script.to_string();
    match guarded(watchdog, move || run_inner(&owned, kind)) {
        Ok(outcome) => outcome,
        Err(outcome) => outcome,
    }
}

/// Runs `work` in its own thread (16 MiB stack, like a generous host thread) under
/// `catch_unwind` and a watchdog: `Err(Outcome::Panic)` or `Err(Outcome::Hang)` when it does not
/// return normally. A hung thread is leaked on purpose (it cannot be killed).
pub(crate) fn guarded<T: Send + 'static>(
    watchdog: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Outcome> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("robustness-case".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(work)).map_err(|payload| {
                let message = if let Some(text) = payload.downcast_ref::<&str>() {
                    (*text).to_string()
                } else if let Some(text) = payload.downcast_ref::<String>() {
                    text.clone()
                } else {
                    "<non-string panic payload>".to_string()
                };
                Outcome::Panic(message)
            });
            let _ = tx.send(result);
        });
    if spawned.is_err() {
        return Err(Outcome::Error("<could not spawn the case thread>".into()));
    }
    rx.recv_timeout(watchdog).unwrap_or(Err(Outcome::Hang))
}

/// Findings derived from an outcome, as `(kind, signature)` pairs.
pub(crate) fn findings_of(outcome: &Outcome) -> Vec<(&'static str, String)> {
    match outcome {
        Outcome::Ok => vec![],
        Outcome::Panic(message) => vec![("panic", normalize(message))],
        Outcome::Hang => vec![("hang", String::new())],
        Outcome::Error(text) => {
            let mut found = vec![];
            if text.contains("execution failed") && !text.contains("at #") {
                let head = text.lines().next().unwrap_or("").to_string();
                found.push(("unlocated", normalize(&head)));
            } else if !text.contains("at #") && !text.contains("(line ") && !has_no_step(text) {
                // Neither a step location nor a script position: the user cannot tell where.
                let head = text.lines().next().unwrap_or("").to_string();
                found.push(("no-location", normalize(&head)));
            }
            if let Some(marker) = debug_marker(text) {
                found.push(("debug-leak", marker.to_string()));
            }
            // The timeout fired, but far too late: some step does long work between checks.
            if let Some(ran) = text
                .split("(ran ")
                .nth(1)
                .and_then(|rest| rest.split(" ms").next())
                .and_then(|ms| ms.parse::<i64>().ok())
                && ran > 3 * EVALUATION_TIMEOUT_MS
            {
                let head = text.lines().next().unwrap_or("").to_string();
                found.push(("timeout-overshoot", normalize(&head)));
            }
            // A Rust type path (`graphersal::prelude::Operator`) in a user-facing error.
            if let Some(at) = text.find("graphersal::") {
                let path: String = text[at..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == ':' || *c == '_')
                    .collect();
                found.push(("type-leak", path));
            } else if let Some(name) = rust_primitive_name(text) {
                // A Rust primitive (`got a f64`, `(GraphTraversalSource, i64)`) instead of the
                // display name (`float64`, `int64`).
                found.push(("type-leak", name.to_string()));
            }
            found
        }
    }
}

/// Diagnostics of the errors that have no step to locate and name what they are about
/// instead, so they need no `at #N`:
/// - an invalid or locked option (`g.with("key", ..)`, the options map of `execute()`): the
///   option is set on the source or the terminal, not on a step, and the message names its key;
/// - a denied `with("key")` option: the same, under the host's authorizer.
///
/// Every other error must carry a step location or a script position.
const NO_STEP_ERRORS: &[&str] = &[
    "Error: Invalid value for option '",
    "Error: Option '",
    "Error: Permission denied for 'with(\"",
];

fn has_no_step(text: &str) -> bool {
    NO_STEP_ERRORS.iter().any(|prefix| text.starts_with(prefix))
}

/// A Rust primitive type name as a whole word (`f64`, `i64`, `ImmutableString`).
fn rust_primitive_name(text: &str) -> Option<&'static str> {
    let words = text.split(|c: char| !(c.is_alphanumeric() || c == '_'));
    for word in words {
        for name in ["i64", "f64", "ImmutableString", "usize"] {
            if word == name {
                return Some(name);
            }
        }
    }
    None
}

/// Rust `Debug` fragments that must never appear in a user-facing diagnostic.
const DEBUG_MARKERS: &[&str] = &[
    "Some(",
    "String(\"",
    "Int64(",
    "Float64(",
    "Boolean(",
    "Object({",
    "Array([",
    "ElementProperty",
    "TraverserValue",
    "GraphNodeIndex",
    "NodeIndex(",
    "CompactString",
    "PropKey",
    ": None",
    "Vertex(",
];

fn debug_marker(text: &str) -> Option<&'static str> {
    // `UUID.fromString("..")` in a help text is DSL, not the `Debug` form `String("..")`.
    let text = text.replace("fromString(\"", "fromString(");
    DEBUG_MARKERS.iter().copied().find(|m| text.contains(m))
}

/// A signature stable across seeds: digits collapsed.
fn normalize(text: &str) -> String {
    let mut out = String::new();
    let mut last_digit = false;
    for c in text.chars().take(200) {
        if c.is_ascii_digit() {
            if !last_digit {
                out.push('N');
            }
            last_digit = true;
        } else {
            out.push(c);
            last_digit = false;
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Minimization.
// ---------------------------------------------------------------------------------------------

fn same_finding(
    script: &Script,
    kind: GraphKind,
    wanted: &(&'static str, String),
    watchdog: Duration,
) -> bool {
    let outcome = run_case(&script.render(), kind, watchdog);
    findings_of(&outcome).iter().any(|f| f == wanted)
}

/// All one-step simplifications of a call list (remove a call, simplify or drop an argument,
/// inline a nested traversal).
fn shrink_calls(calls: &[Call]) -> Vec<Vec<Call>> {
    let mut out = vec![];
    for i in 0..calls.len() {
        let mut removed = calls.to_vec();
        removed.remove(i);
        out.push(removed);
    }
    for i in 0..calls.len() {
        for j in 0..calls[i].args.len() {
            let mut fewer = calls.to_vec();
            fewer[i].args.remove(j);
            out.push(fewer);
            match &calls[i].args[j] {
                Arg::Lit(text) => {
                    for simple in ["\"x\"", "1"] {
                        if text != simple {
                            let mut simpler = calls.to_vec();
                            simpler[i].args[j] = Arg::Lit(simple.to_string());
                            out.push(simpler);
                        }
                    }
                }
                Arg::Anon(inner) => {
                    for shrunk in shrink_calls(inner) {
                        let mut nested = calls.to_vec();
                        nested[i].args[j] = Arg::Anon(shrunk);
                        out.push(nested);
                    }
                }
            }
        }
    }
    out
}

/// Greedy structural minimization that keeps `wanted` reproducing.
pub(crate) fn minimize(
    script: &Script,
    kind: GraphKind,
    wanted: &(&'static str, String),
    watchdog: Duration,
    budget: usize,
) -> Script {
    let mut best = script.clone();
    let mut tries = 0;
    // Simpler terminal and source first.
    for terminal in [".toList()", ""] {
        let candidate = Script {
            terminal: terminal.into(),
            ..best.clone()
        };
        if candidate.terminal != best.terminal && same_finding(&candidate, kind, wanted, watchdog) {
            best = candidate;
        }
    }
    for source in ["g.v()", "g.inject(1)"] {
        let candidate = Script {
            source: source.into(),
            ..best.clone()
        };
        if candidate.source != best.source && same_finding(&candidate, kind, wanted, watchdog) {
            best = candidate;
        }
    }
    'outer: loop {
        for steps in shrink_calls(&best.steps) {
            tries += 1;
            if tries > budget {
                break 'outer;
            }
            let candidate = Script {
                steps,
                ..best.clone()
            };
            if same_finding(&candidate, kind, wanted, watchdog) {
                best = candidate;
                continue 'outer;
            }
        }
        break;
    }
    best
}

// ---------------------------------------------------------------------------------------------
// The campaign.
// ---------------------------------------------------------------------------------------------

/// Panic messages of known, documented findings: each one has an ignored `BUG:` regression test
/// in `robustness_bug_tests.rs`. The fast budget tolerates them so it stays green until the fix
/// lands; remove the entry together with the `#[ignore]`.
const KNOWN_PANICS: &[(&str, &str)] = &[];

/// One recorded finding of a campaign.
#[derive(Clone, Debug)]
struct Finding {
    kind: &'static str,
    signature: String,
    seed: u64,
    graph: GraphKind,
    script: String,
    minimized: String,
    count: u64,
}

struct Campaign {
    cases: u64,
    seed: u64,
    threads: usize,
    watchdog: Duration,
    minimize: bool,
    verbose: bool,
}

#[derive(Default)]
struct Totals {
    ok: AtomicU64,
    errors: AtomicU64,
    runs: AtomicU64,
}

fn run_campaign(campaign: &Campaign) -> (Vec<Finding>, u64, u64, u64) {
    let findings: Mutex<BTreeMap<(&'static str, String), Finding>> = Mutex::new(BTreeMap::new());
    let totals = Totals::default();
    let next = AtomicU64::new(0);
    let hangs = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..campaign.threads.max(1) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= campaign.cases {
                        break;
                    }
                    let seed = campaign.seed.wrapping_mul(1_000_003).wrapping_add(i);
                    let script = generate(seed);
                    let text = script.render();
                    for kind in [GraphKind::Modern, GraphKind::Empty] {
                        if hangs.load(Ordering::Relaxed) > 4 {
                            // Every hang leaks a spinning thread; stop before they eat the machine.
                            return;
                        }
                        let outcome = run_case(&text, kind, campaign.watchdog);
                        if campaign.verbose {
                            let head = match &outcome {
                                Outcome::Error(e) => e.lines().next().unwrap_or("").to_string(),
                                other => format!("{other:?}"),
                            };
                            println!("{seed} {kind:?} {text}\n    -> {head}");
                        }
                        totals.runs.fetch_add(1, Ordering::Relaxed);
                        match &outcome {
                            Outcome::Ok => totals.ok.fetch_add(1, Ordering::Relaxed),
                            Outcome::Hang => hangs.fetch_add(1, Ordering::Relaxed) as u64,
                            _ => totals.errors.fetch_add(1, Ordering::Relaxed),
                        };
                        for (fkind, signature) in findings_of(&outcome) {
                            let key = (fkind, signature.clone());
                            let fresh = {
                                let mut map = findings.lock().unwrap_or_else(|e| e.into_inner());
                                match map.get_mut(&key) {
                                    Some(found) => {
                                        found.count += 1;
                                        false
                                    }
                                    None => {
                                        map.insert(
                                            key.clone(),
                                            Finding {
                                                kind: fkind,
                                                signature,
                                                seed,
                                                graph: kind,
                                                script: text.clone(),
                                                minimized: text.clone(),
                                                count: 1,
                                            },
                                        );
                                        true
                                    }
                                }
                            };
                            if fresh && campaign.minimize && fkind != "hang" {
                                let small = minimize(&script, kind, &key, campaign.watchdog, 400);
                                let mut map = findings.lock().unwrap_or_else(|e| e.into_inner());
                                if let Some(found) = map.get_mut(&key) {
                                    found.minimized = small.render();
                                }
                            }
                        }
                    }
                }
            });
        }
    });
    let map = findings.into_inner().unwrap_or_else(|e| e.into_inner());
    (
        map.into_values().collect(),
        totals.runs.load(Ordering::Relaxed),
        totals.ok.load(Ordering::Relaxed),
        totals.errors.load(Ordering::Relaxed),
    )
}

fn report(findings: &[Finding], runs: u64, ok: u64, errors: u64, elapsed: Duration) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "robustness campaign: {runs} runs ({ok} ok, {errors} clean errors) in {:.1?}; {} distinct findings",
        elapsed,
        findings.len()
    );
    for f in findings {
        let _ = writeln!(
            out,
            "\n[{}] x{} seed={} graph={:?}\n  signature: {}\n  minimized: {}\n  original:  {}",
            f.kind, f.count, f.seed, f.graph, f.signature, f.minimized, f.script
        );
    }
    out
}

/// Hand-written edge probes: arithmetic at the `i64` borders, NaN/inf, extreme slice bounds,
/// deep nesting and loop shapes that random generation rarely reaches. Each runs on both graphs.
const PROBES: &[&str] = &[
    "g.inject(imax, 1).sum().next()",
    "g.inject([imax, 1]).sum(Scope.local).next()",
    "g.inject(imax, imax).mean().next()",
    "g.inject(imin, -1).sum().next()",
    "g.withSack(imax).inject(1).sack(Operator.mult).by(__.constant(2)).sack().next()",
    "g.withSack(imax).inject(1).sack(Operator.sum).by(__.constant(1)).sack().next()",
    "g.withSack(imin).inject(1).sack(Operator.div).by(__.constant(-1)).sack().next()",
    "g.withSack(imin).inject(1).sack(Operator.minus).by(__.constant(1)).sack().next()",
    "g.withSack(imax).inject(1).sack(Operator.sumLong).by(__.constant(1)).sack().next()",
    "g.withSack(1).inject(1).sack(Operator.div).by(__.constant(0)).sack().next()",
    "g.inject(imin).math(\"-_\").next()",
    "g.inject(imin).math(\"abs(_)\").next()",
    "g.inject(imax).math(\"_ * _\").next()",
    "g.inject(imax).as_number(GType.INT).next()",
    "g.inject(1e308).as_number(GType.LONG).next()",
    "g.inject(nan).as_number(GType.LONG).next()",
    "g.inject(inf).as_number(GType.INT).next()",
    "g.inject(\"9223372036854775808\").as_number(GType.LONG).next()",
    "g.inject(\"abcdef\").substring(imin, imax).next()",
    "g.inject(\"abcdef\").substring(imax).next()",
    "g.inject(\"abcdef\").substring(imin).next()",
    "g.inject(\"abcdef\").substring(-3, imin).next()",
    "g.inject(\"äöü😀\").substring(1, 2).next()",
    "g.inject(\"äöü😀\").substring(-1).next()",
    "g.v().range(imax, imax).toList()",
    "g.v().range(imin, imin).toList()",
    "g.v().range(0, imax).toList()",
    "g.v().tail(imax).toList()",
    "g.v().skip(imax).toList()",
    "g.v().limit(imin).toList()",
    "g.v().fold().range(Scope.local, imax, -1).toList()",
    "g.v().fold().tail(Scope.local, imax).toList()",
    "g.v().fold().limit(Scope.local, imin).toList()",
    "g.v().fold().skip(Scope.local, imax).toList()",
    "g.v().values(\"name\").fold().range(Scope.local, imax - 1, imax).toList()",
    "g.v().coin(nan).count().next()",
    "g.v().coin(inf).count().next()",
    "g.v().coin(ninf).count().next()",
    "g.inject(1).fold(imax, Operator.sum).next()",
    "g.inject(imax).fold(1, Operator.sumLong).next()",
    "g.inject(imax, 2).fold(1, Operator.mult).next()",
    "g.inject(imin).fold(-1, Operator.div).next()",
    "g.inject(imin, 2).fold(0, Operator.minus).next()",
    "g.inject(1).barrier(imax).toList()",
    "g.inject(1).barrier(0).toList()",
    "g.v().has_id(imin).toList()",
    "g.v(imax).toList()",
    "g.inject(imin).as_string().next()",
    "g.inject(1e308, 1e308).sum().next()",
    "g.inject(1e308, 1e308).mean().next()",
    "g.inject(nan, 1).max().next()",
    "g.inject(nan, 1).order().toList()",
    "g.inject(nan, nan).dedup().toList()",
    "g.inject(nan).group_count().next()",
    "g.inject(imin).is(P.between(imax, imin)).toList()",
    "g.inject(1).repeat(__.inject(1)).times(imax).limit(1).toList()",
    "g.v().repeat(__.both()).times(30).count().next()",
    "g.v().repeat(__.both()).emit().times(12).path().count().next()",
    "g.v().repeat(__.both()).until(__.has(\"name\", \"nobody\")).count().next()",
    "g.v().repeat(__.both().simplePath()).toList()",
    "g.with(\"repeat.order\", \"dfs\").v().repeat(__.both()).times(9).count().next()",
    "g.with(\"repeat.order\", \"dfs\").v().repeat(__.both()).emit().toList()",
    "g.v().repeat(__.out()).times(0).toList()",
    "g.v().repeat(__.repeat(__.both()).times(3)).times(3).count().next()",
    "g.v().repeat(__.both().loops()).times(3).toList()",
    "g.v().emit().repeat(__.both()).until(__.loops().is(P.gt(4))).count().next()",
    "g.v().both().both().both().both().both().both().both().both().both().both().both().both().both().both().both().both().count().next()",
    "g.v().both().both().both().both().both().both().both().both().both().both().both().both().path().count().next()",
    "g.v().both().both().both().both().both().both().both().both().both().both().fold().count(Scope.local).next()",
    "g.v().as_(\"a\").both().as_(\"a\").both().as_(\"a\").select(Pop.all, \"a\").toList()",
    "g.v().as_(\"a\").select(\"a\").select(\"a\").select(\"a\").toList()",
    "g.v().select(\"a\").toList()",
    "g.v().math(\"_ / 0\").toList()",
    "g.v().values(\"age\").math(\"_ / 0\").toList()",
    "g.v().values(\"age\").math(\"_ % 0\").toList()",
    "g.v().values(\"age\").math(\"_ ^ 1e308\").toList()",
    "g.v().values(\"age\").math(\"((((((((((((((((((((((((((((((((((((((((_))))))))))))))))))))))))))))))))))))))))\").toList()",
    "g.v().values(\"age\").math(\"-------------------------------------------------------------------------_\").toList()",
    "g.inject(\"\").glob_path(\"**/**/**/**/**/**/**/**\").toList()",
    "g.inject(#{a: #{a: #{a: #{a: #{a: [1, [2, [3, [4]]]]}}}}}).json_path(\"$..a\").toList()",
    "g.inject(#{a: [1, 2, 3]}).json_path(\"$.a[-9223372036854775807]\").toList()",
    "g.inject(1).constant(big_list).unfold().count().next()",
    "g.inject(big_list).unfold().fold().unfold().dedup().count().next()",
    "g.inject(big_string).toUpper().length().next()",
    "g.addV(\"a\").property(\"k\", big_list).values(\"k\").count().next()",
    "g.addV(\"a\").property(\"k\", nan).values(\"k\").toList()",
    "g.addV(\"a\").property(\"k\", #{a: #{b: #{c: [nan, inf, imin]}}}).valueMap().toList()",
    "g.addV(big_string).label().length().next()",
    "g.addV(\"\").label().toList()",
    "g.v().property(\"name\", ()).values(\"name\").toList()",
    "g.v().drop().v().count().next()",
    "g.v().as_(\"a\").out().drop().select(\"a\").toList()",
    "g.e().drop().inject(1).v().both().toList()",
    "g.v().out_e().as_(\"e\").in_v().drop().select(\"e\").out_v().toList()",
    "g.v().addE(\"x\").to(__.v()).count().next()",
    "g.v().addE(\"x\").to(__.v()).from(__.v()).count().next()",
    "g.v().addE(\"x\").to(__.v().drop()).toList()",
    "g.v().out_e().property(\"weight\", __.in_v().drop()).toList()",
    "g.v(1).property(\"name\", __.v(1).drop()).toList()",
    "g.mergeV(#{\"T.id\": imin}).toList()",
    "g.mergeV(#{\"T.id\": nan}).toList()",
    "g.mergeV(#{\"T.label\": \"\"}).toList()",
    "g.mergeE(#{\"Direction.OUT\": 1, \"Direction.IN\": 1, \"T.label\": \"self\"}).toList()",
    "g.mergeE(#{\"Direction.OUT\": 999, \"Direction.IN\": 1}).toList()",
    "g.mergeE(#{\"Direction.OUT\": nan, \"Direction.IN\": imin}).toList()",
    "g.v().groupCount().by(__.both().both().both().count()).toList()",
    "g.v().group().by(__.identity()).by(__.both().fold()).unfold().select(Column.values).unfold().unfold().toList()",
    "g.v().project(\"a\", \"a\").by(\"name\").by(\"age\").toList()",
    "g.v().project(\"a\").toList()",
    "g.v().order().by(__.both().count(), Order.desc).by(\"nonexistent\").toList()",
    "g.v().order().by(\"name\", Order.shuffle).toList()",
    "g.v().path().by(\"name\").by(__.both().both().fold()).toList()",
    "g.v().tree().by(\"name\").toList()",
    "g.v().both().both().both().tree().toList()",
    "g.v().both().both().both().both().both().both().both().both().both().both().both().both().both().both().both().both().both().both().both().both().both().both().both().both().both().both().both().both().both().both().count().next()",
    "g.v().local(__.both().both().both().both().both().both().both().both().fold()).count().next()",
    "g.v().union(__.union(__.union(__.union(__.union(__.union(__.union(__.union(__.both())))))))).count().next()",
    "g.v().where(__.where(__.where(__.where(__.where(__.where(__.where(__.where(__.both())))))))).count().next()",
    "g.v().sack().toList()",
    "g.withSack(#{a: [1]}).v().sack(Operator.addAll).by(__.constant([2])).sack().toList()",
    "g.withSack([], Operator.addAll).v().both().both().barrier().sack().toList()",
    "g.withSack(1.0, Operator.mult).v().repeat(__.both().sack(Operator.mult).by(__.constant(1e300))).times(4).sack().toList()",
    "g.withSideEffect(\"a\", big_list).v().cap(\"a\").next()",
    "g.v().aggregate(\"a\").both().where(P.within(\"a\")).count().next()",
    "g.v().aggregate(\"a\").cap(\"a\").unfold().aggregate(\"a\").cap(\"a\").count(Scope.local).next()",
    "g.v().store(\"a\").cap(\"b\").next()",
    "g.v().cap(\"a\").next()",
    "g.v().subgraph(\"s\").cap(\"s\").next()",
    "g.v().both().subgraph(\"s\").both().subgraph(\"s\").cap(\"s\").next()",
    "g.v().choose(__.values(\"age\"), __.out(), __.in_()).toList()",
    "g.v().choose(__.values(\"age\")).option(29, __.out()).option(29, __.in_()).toList()",
    "g.v().branch(__.label()).option(Pick.any, __.both()).option(Pick.none, __.both()).option(Pick.unproductive, __.both()).count().next()",
    "g.v().with_side_effect(\"x\", 1).toList()",
    "g.v().inject(()).toList()",
    "g.inject(()).as_number().toList()",
    "g.inject(()).unfold().toList()",
    "g.inject(()).values(\"x\").toList()",
    "g.inject(()).out().toList()",
    "g.inject([], #{}).unfold().toList()",
    "g.inject(#{}).select(Column.keys).toList()",
    "g.inject(#{a: 1}).unfold().select(Column.values).toList()",
    "g.inject(#{a: 1}).unfold().unfold().unfold().toList()",
    "g.inject(UUID.fromString(\"00000000-0000-0000-0000-000000000000\")).as_number().toList()",
    "g.inject(UUID.fromString(\"00000000-0000-0000-0000-000000000000\")).math(\"_ + 1\").toList()",
    "g.v().has(\"age\", P.within(big_list)).count().next()",
    "g.v().has(\"age\", P.between(nan, inf)).count().next()",
    "g.v().has(\"name\", TextP.containing(big_string)).count().next()",
    "g.v().has(\"name\", TextP.startingWith(\"\")).count().next()",
    "g.v().has(jpath(\"a[-1]\"), 1).toList()",
    "g.v().values(jpath(\"name[0]\")).toList()",
    "g.v().property(jpath(\"a.b[3].c\"), 1).values(jpath(\"a\")).toList()",
    "g.v().property(jpath(\"a[1000000]\"), 1).values(jpath(\"a\")).toList()",
    "g.v().property(jpath(\"a[-1]\"), 1).values(jpath(\"a\")).toList()",
    "g.v().property(jpath(\"name.x\"), 1).values(\"name\").toList()",
    "g.v().remove_property(jpath(\"name[0]\")).toList()",
    "g.v().properties(jpath(\"name\")).drop().toList()",
    "g.v().properties(\"name\").properties(\"x\").toList()",
    "g.v().properties().property(\"meta\", 1).toList()",
    "g.v().values(\"name\").drop().toList()",
    "g.v().as_(\"a\").both().as_(\"b\").select(\"a\", \"b\").by(__.both().both().fold()).count().next()",
    "g.v().as_(\"a\").where(\"a\", P.eq(\"b\")).toList()",
    "g.v().as_(\"a\").both().where(P.gt(\"a\")).by(\"age\").toList()",
    "g.v().math(\"a + b\").by(\"age\").toList()",
    "g.v().as_(\"a\").math(\"a + a + a\").by(\"age\").toList()",
    "g.v().loops().toList()",
    "g.v().loops(\"nope\").toList()",
    "g.v().times(3).toList()",
    "g.v().until(__.out()).toList()",
    "g.v().emit().toList()",
    "g.v().repeat(__.out()).repeat(__.out()).toList()",
    "g.v().repeat(__.times(2)).toList()",
    "g.v().repeat(__.emit()).times(2).toList()",
    "g.v().repeat(__.until(__.out())).times(2).toList()",
    "g.v().repeat(__.repeat(__.out())).times(2).toList()",
    "g.v().by(\"name\").toList()",
    "g.v().option(1, __.out()).toList()",
    "g.v().from(\"a\").toList()",
    "g.v().as_(\"\").select(\"\").toList()",
    "g.v().simplePath().cyclicPath().path().toList()",
    "g.inject(1).path().unfold().path().unfold().path().toList()",
    "g.v().element_map().unfold().select(Column.keys).toList()",
    "g.v().value_map(true).toList()",
    "g.e().element_map(\"weight\").toList()",
    "g.v().both_e().other_v().other_v().toList()",
    "g.inject(1).other_v().toList()",
    "g.v().properties().element().element().toList()",
    "g.v().key().toList()",
    "g.e().properties().key().value().toList()",
    "g.v().profile_with(ProfileType.Memory)",
    "g.v().both().both().both().group().by(__.label()).profile_with(ProfileType.Memory)",
    "g.v().execute(#{profile: true, profile_types: [ProfileType.Memory]})",
    "g.v().execute(#{profile: imin})",
    "g.v().to_table(#{max_rows: imin})",
    "g.v().to_table(#{max_rows: 0, max_items: imax})",
    "g.v().to_json(#{max_rows: -1})",
    "g.v().visualize(V.Mermaid, #{max_rows: imax})",
    "g.v().to_tree(#{no_limit: 1})",
    "g.v().both().both().both().both().both().both().both().both().to_table()",
    "g.v().path().to_mermaid()",
    "g.v().both().both().path().to_plantuml()",
    "g.v().schema()",
    "g.v().statistics()",
    "g.memory_usage()",
    "g.v().to_graph()",
    "g.e().to_graph().v().count().next()",
    "g.with(\"bulk.max_expansion\", 0).v().both().both().barrier().toList()",
    "g.with(\"bulk.max_expansion\", imin).v().toList()",
    "g.with(\"traversal.max_traversers\", 0).v().toList()",
    "g.with(\"traversal.max_traversers\", imin).v().toList()",
    "g.with(\"repeat.max_loops\", 0).v().repeat(__.both()).toList()",
    "g.with(\"repeat.max_loops\", imax).v().repeat(__.both()).limit(1).toList()",
    "g.with(\"repeat.max_loops\", -1).v().repeat(__.both()).toList()",
    "g.with(\"evaluationTimeout\", imin).v().toList()",
    "g.with(\"evaluationTimeout\", nan).v().toList()",
    "g.with(\"evaluationTimeout\", 1e308).v().toList()",
    "g.with(\"optimizer.disabled\", [big_string]).v().toList()",
    "g.with(\"optimizer.enabled\", [\"count_pushdown\"]).with(\"optimizer.disabled\", [\"count_pushdown\"]).v().count().next()",
    "g.with(\"bulk.one\", true).withSack(1, Operator.sum).v().both().both().barrier().sack().toList()",
    "g.withBulk(false).v().both().both().barrier().count().next()",
    "g.inject(1).with(\"evaluationTimeout\", 0).toList()",
];

/// Runs every probe on both graphs; fails on any finding but a timeout overshoot.
#[test]
fn edge_probes() {
    let verbose = std::env::var("ROBUSTNESS_VERBOSE").is_ok();
    let fatal = Mutex::new(vec![]);
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(probe) = PROBES.get(i) else { break };
                    for kind in [GraphKind::Modern, GraphKind::Empty] {
                        let started = Instant::now();
                        let outcome = run_case(probe, kind, Duration::from_secs(60));
                        if verbose {
                            let ms = started.elapsed().as_millis();
                            println!("{kind:?} {ms}ms {probe}\n    -> {outcome:?}");
                        }
                        for (fkind, signature) in findings_of(&outcome) {
                            let known = KNOWN_PANICS.iter().any(|(message, script)| {
                                signature.contains(message) && probe.contains(script)
                            });
                            if fkind != "timeout-overshoot" && !known {
                                let mut list = fatal.lock().unwrap_or_else(|e| e.into_inner());
                                list.push(format!("[{fkind}] {kind:?} {probe}: {signature}"));
                            }
                        }
                    }
                }
            });
        }
    });
    let fatal = fatal.into_inner().unwrap_or_else(|e| e.into_inner());
    assert!(fatal.is_empty(), "{}", fatal.join("\n"));
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// The surface parser must find the step table; an empty table would make the fuzzer vacuous.
#[test]
fn step_table_covers_the_surface() {
    let table = step_table();
    let names: HashSet<&str> = table.iter().map(|(n, _)| n.as_str()).collect();
    for step in [
        "v", "out", "has", "repeat", "times", "property", "drop", "select", "math",
    ] {
        assert!(names.contains(step), "step table misses {step}: {names:?}");
    }
    assert!(table.len() > 150, "only {} steps parsed", table.len());
}

/// The generator calls `gql(string)` and `gql(string, map)` too (feature `gql`): both arities
/// come from the surface, so random query strings and parameter values reach the GQL parser,
/// the lowering and the start-only check.
#[test]
#[cfg(feature = "gql")]
fn step_table_covers_gql() {
    let table = step_table();
    let arities = table
        .iter()
        .find(|(name, _)| name == "gql")
        .map(|(_, arities)| arities.clone())
        .unwrap_or_default();
    assert!(
        arities.contains(&1) && arities.contains(&2),
        "gql arities in the step table: {arities:?}"
    );
}

/// A case's files stay in its own temporary directory: a relative name is written there, a path
/// that leaves it is denied with a located diagnostic (so the denial is no finding), and nothing
/// reaches the working directory.
#[test]
fn case_files_stay_in_the_temporary_directory() {
    let watchdog = Duration::from_secs(60);
    let name = "graphersal-fuzz-file-root-probe.graphml";
    assert!(
        matches!(
            run_case(
                &format!(r#"g.export_graphml("{name}")"#),
                GraphKind::Modern,
                watchdog
            ),
            Outcome::Ok
        ),
        "a relative name is written inside the file root"
    );
    assert!(
        !std::path::Path::new(name).exists(),
        "nothing in the working directory"
    );
    for path in ["../escape.graphml", "/tmp/graphersal-fuzz-escape.graphml"] {
        let outcome = run_case(
            &format!(r#"g.export_graphml("{path}")"#),
            GraphKind::Modern,
            watchdog,
        );
        let Outcome::Error(text) = &outcome else {
            panic!("{path}: expected a denial, got {outcome:?}");
        };
        assert!(text.contains("Permission denied"), "{path}: {text}");
        assert!(
            findings_of(&outcome).is_empty(),
            "{path}: {:?}\n{text}",
            findings_of(&outcome)
        );
    }
    assert!(!std::path::Path::new("/tmp/graphersal-fuzz-escape.graphml").exists());
}

/// The same seed builds the same script (reproducibility of every reported finding).
#[test]
fn generation_is_deterministic() {
    for seed in 0..50 {
        assert_eq!(generate(seed).render(), generate(seed).render());
    }
}

/// The fast budget: a fixed seed and a small number of cases, in every test run.
#[test]
fn fuzz_fast() {
    let campaign = Campaign {
        cases: 200,
        seed: 20_261_006,
        threads: 4,
        watchdog: Duration::from_secs(30),
        minimize: false,
        verbose: std::env::var("ROBUSTNESS_VERBOSE").is_ok(),
    };
    let start = Instant::now();
    let (findings, runs, ok, errors) = run_campaign(&campaign);
    // Every finding is fatal (the classes are 0 since RB3) except a timeout overshoot, which
    // depends on the machine's load in a debug build.
    let fatal: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.kind != "timeout-overshoot")
        .filter(|f| {
            !KNOWN_PANICS
                .iter()
                .any(|(message, script)| f.signature.contains(message) && f.script.contains(script))
        })
        .collect();
    assert!(
        fatal.is_empty(),
        "{}",
        report(&findings, runs, ok, errors, start.elapsed())
    );
    // Sanity: the generator must produce a healthy share of scripts that actually run.
    assert!(
        ok > runs / 20,
        "only {ok} of {runs} runs succeeded: the generator is too noisy"
    );
}

/// Seeds of the long campaign whose unbounded `repeat()` grew to gigabytes within the 1 s
/// `evaluationTimeout` (`repeat(__.constant([..]).unfold())`, `repeat(__.group().barrier())`,
/// `repeat(__.path())`): under the host's `memory.limit` each stops with the memory limit error.
/// The `evaluationTimeout` is raised so a debug build reaches the limit before the deadline, and
/// the limit is lowered to 64 MiB to keep the test small: the engine's estimate (no tracking
/// allocator here) undercounts these loops several times over, so a 512 MiB host limit let one
/// case reach about 3 GB of resident memory.
#[test]
fn unbounded_repeat_stops_at_the_memory_limit() {
    for seed in [23_102_892, 23_189_523, 23_159_444] {
        let script = generate(seed).render();
        for kind in [GraphKind::Modern, GraphKind::Empty] {
            let start = Instant::now();
            let owned = script.clone();
            let outcome = guarded(Duration::from_secs(60), move || {
                run_inner_with(&owned, kind, 60_000, 64 * 1024 * 1024)
            })
            .unwrap_or_else(|outcome| outcome);
            let elapsed = start.elapsed();
            assert!(
                matches!(&outcome, Outcome::Error(text) if text.contains("'memory.limit' exceeded")),
                "seed {seed} on {kind:?}: {script}\n  -> {outcome:?}"
            );
            assert!(
                elapsed < Duration::from_secs(30),
                "seed {seed} on {kind:?} took {elapsed:?}"
            );
        }
    }
}

/// The long budget (opt-in, see the module docs). Never fails on findings: it reports them.
#[test]
#[ignore = "long robustness campaign; run with ROBUSTNESS_CASES=<n> -- --ignored"]
fn fuzz_long() {
    if let Ok(seed) = std::env::var("ROBUSTNESS_REPLAY") {
        let seed: u64 = seed.parse().unwrap_or(0);
        let script = generate(seed).render();
        for kind in [GraphKind::Modern, GraphKind::Empty] {
            let outcome = run_case(&script, kind, Duration::from_secs(30));
            println!("{kind:?}: {script}\n  -> {outcome:?}");
        }
        return;
    }
    let campaign = Campaign {
        cases: env_u64("ROBUSTNESS_CASES", 5_000),
        seed: env_u64("ROBUSTNESS_SEED", 1),
        threads: env_u64("ROBUSTNESS_THREADS", 8) as usize,
        watchdog: Duration::from_secs(env_u64("ROBUSTNESS_WATCHDOG_SECS", 20)),
        minimize: true,
        verbose: std::env::var("ROBUSTNESS_VERBOSE").is_ok(),
    };
    let start = Instant::now();
    let (findings, runs, ok, errors) = run_campaign(&campaign);
    let text = report(&findings, runs, ok, errors, start.elapsed());
    let path = std::env::var("ROBUSTNESS_OUT").unwrap_or_else(|_| {
        format!(
            "{}/../../target/robustness-findings.txt",
            env!("CARGO_MANIFEST_DIR")
        )
    });
    if let Err(err) = std::fs::write(&path, &text) {
        println!("could not write {path}: {err}");
    }
    println!("{text}");
}
