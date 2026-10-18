//! Snapshot of the whole registered Rhai surface of [`engine`]: every function with its
//! metadata (parameter names and types, return type, namespace, doc comments), the signatures
//! that [`registered_step_names`] reads, the static module names and their variables, the
//! scope globals and [`RESERVED_PARAM_NAMES`]. A change of the DSL surface shows up as a
//! reviewable diff of `tests/snapshots/script_surface.txt`; regenerate it with
//! `GRAPHERSAL_UPDATE_SURFACE=1` and review the diff like code.
//!
//! The surface depends on the `io` and `serde` features (`file`, `export_graphml`,
//! `property_json`, ...), so the snapshot is taken with both on (the acceptance feature set),
//! and with `persist` (`export_snapshot`) and `gql` (`gql`), each filtered out of the comparison
//! without its feature.

use super::eval::RESERVED_PARAM_NAMES;
use super::*;
use std::collections::BTreeSet;
use std::fmt::Write as _;

/// Functions registered only with the `persist` feature (on when the snapshot is taken).
const PERSIST_ONLY: &[&str] = &["export_snapshot", "exportSnapshot"];

/// The lines of functions registered only with the `gql` feature (on when the snapshot is
/// taken): the metadata entries, the signatures and the step name.
fn gql_only(line: &str) -> bool {
    line.contains("\"name\":\"gql\"") || line.starts_with("gql(") || line == "gql"
}

/// Path of the committed snapshot.
const SNAPSHOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/snapshots/script_surface.txt"
);

/// One line per function of a metadata `functions` array, `prefix` in front of the name for a
/// static module member. The hashes are not part of the surface; `signature` repeats the
/// signatures section and `isAnonymous`/`type` are constant for native functions.
fn function_lines(functions: &serde_json::Value, prefix: &str, out: &mut BTreeSet<String>) {
    for f in functions.as_array().into_iter().flatten() {
        let mut f = f.clone();
        if let Some(map) = f.as_object_mut() {
            map.retain(|key, _| {
                !key.ends_with("Hash")
                    && !matches!(key.as_str(), "signature" | "isAnonymous" | "type")
            });
        }
        out.insert(format!("{prefix}{f}"));
    }
}

/// The surface as text, sections sorted so the registration order does not matter (only the
/// overload *set* is the surface; Rhai resolves one call to one exact signature).
fn surface() -> String {
    let engine = engine();
    let mut out = String::new();

    let metadata: serde_json::Value = serde_json::from_str(
        &engine
            .gen_fn_metadata_to_json(false)
            .unwrap_or_else(|err| panic!("metadata: {err}")),
    )
    .unwrap_or_else(|err| panic!("metadata json: {err}"));

    out.push_str("## custom types\n");
    let mut custom_types = BTreeSet::new();
    for c in metadata["customTypes"].as_array().into_iter().flatten() {
        custom_types.insert(c.to_string());
    }
    for module in metadata["modules"].as_object().into_iter().flatten() {
        for c in module.1["customTypes"].as_array().into_iter().flatten() {
            custom_types.insert(format!("{}::{c}", module.0));
        }
    }
    for c in custom_types {
        let _ = writeln!(out, "{c}");
    }

    out.push_str("\n## functions (metadata)\n");
    let mut functions = BTreeSet::new();
    function_lines(&metadata["functions"], "", &mut functions);
    let mut module_names = Vec::new();
    for (name, module) in metadata["modules"].as_object().into_iter().flatten() {
        module_names.push(name.clone());
        function_lines(&module["functions"], &format!("{name}::"), &mut functions);
    }
    for f in functions {
        let _ = writeln!(out, "{f}");
    }

    out.push_str("\n## signatures (gen_fn_signatures(false))\n");
    let signatures: BTreeSet<String> = engine.gen_fn_signatures(false).into_iter().collect();
    for s in signatures {
        let _ = writeln!(out, "{s}");
    }

    // Static module variables cannot be listed through the engine, so every module is probed
    // with every name that any token class spells (the getter names of the scope markers, the
    // static module function names and the known variable-only names).
    out.push_str("\n## static modules and their variables\n");
    let mut candidates: BTreeSet<String> = ["id", "label", "Memory"]
        .into_iter()
        .map(String::from)
        .collect();
    for line in engine.gen_fn_signatures(false) {
        let name = line.split('(').next().unwrap_or_default();
        let name = name.rsplit("::").next().unwrap_or_default();
        let name = name.strip_prefix("get$").unwrap_or(name);
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            candidates.insert(name.to_string());
        }
    }
    let graph = Arc::new(<Graph>::default());
    for module in &module_names {
        let _ = writeln!(out, "{module}");
        for name in &candidates {
            let mut scope = graph_scope(graph.clone());
            if let Ok(value) =
                engine.eval_with_scope::<Dynamic>(&mut scope, &format!("{module}::{name}"))
            {
                let type_name = engine.map_type_name(value.type_name());
                let _ = writeln!(out, "  {name}: {type_name}");
            }
        }
    }

    out.push_str("\n## scope globals (graph_scope)\n");
    let scope = graph_scope(graph);
    let mut globals: Vec<String> = scope
        .iter()
        .map(|(name, _, value)| format!("{name}: {}", engine.map_type_name(value.type_name())))
        .collect();
    globals.sort();
    for g in globals {
        let _ = writeln!(out, "{g}");
    }

    out.push_str("\n## RESERVED_PARAM_NAMES\n");
    let mut reserved = RESERVED_PARAM_NAMES.to_vec();
    reserved.sort_unstable();
    for name in reserved {
        let _ = writeln!(out, "{name}");
    }

    out.push_str("\n## registered_step_names()\n");
    for name in registered_step_names() {
        let _ = writeln!(out, "{name}");
    }
    out
}

#[test]
fn registered_surface_matches_the_snapshot() {
    let actual = surface();
    if std::env::var_os("GRAPHERSAL_UPDATE_SURFACE").is_some() {
        if let Some(dir) = std::path::Path::new(SNAPSHOT).parent() {
            std::fs::create_dir_all(dir).unwrap_or_else(|err| panic!("{err}"));
        }
        std::fs::write(SNAPSHOT, &actual).unwrap_or_else(|err| panic!("{err}"));
        return;
    }
    let mut expected = std::fs::read_to_string(SNAPSHOT).unwrap_or_default();
    if !cfg!(feature = "persist") {
        // The snapshot is taken with `persist` on; without it the snapshot functions are not
        // registered, everything else is the same.
        expected = expected
            .lines()
            .filter(|line| !PERSIST_ONLY.iter().any(|name| line.contains(name)))
            .map(|line| format!("{line}\n"))
            .collect();
    }
    if !cfg!(feature = "gql") {
        // The snapshot is taken with `gql` on; without it `gql()` is not registered.
        expected = expected
            .lines()
            .filter(|line| !gql_only(line))
            .map(|line| format!("{line}\n"))
            .collect();
    }
    if actual != expected {
        let expected_lines: BTreeSet<&str> = expected.lines().collect();
        let actual_lines: BTreeSet<&str> = actual.lines().collect();
        let missing: Vec<&&str> = expected_lines.difference(&actual_lines).take(40).collect();
        let added: Vec<&&str> = actual_lines.difference(&expected_lines).take(40).collect();
        panic!(
            "the registered Rhai surface changed (regenerate with GRAPHERSAL_UPDATE_SURFACE=1 \
             and review the diff of {SNAPSHOT}).\nmissing (first 40): {missing:#?}\nadded \
             (first 40): {added:#?}"
        );
    }
}

#[test]
fn no_registration_replaces_an_earlier_one() {
    // A later registration of the same name and parameter types silently replaces the earlier
    // one, so registration order would become part of the surface.
    assert_eq!(super::register::replaced_signatures(), Vec::<String>::new());
}

#[test]
fn standard_library_is_the_one_of_engine_new() {
    // `engine()` attaches a shared standard package instead of building `Engine::new()`.
    let ours = engine();
    let all: BTreeSet<String> = ours.gen_fn_signatures(true).into_iter().collect();
    let dsl: BTreeSet<String> = ours.gen_fn_signatures(false).into_iter().collect();
    let standard: BTreeSet<String> = all.difference(&dsl).cloned().collect();
    let expected: BTreeSet<String> = rhai::Engine::new()
        .gen_fn_signatures(true)
        .into_iter()
        .collect();
    assert_eq!(standard, expected);
}

/// Names registered in one spelling only, on purpose (rule 2 asks for both spellings of a step).
const SINGLE_SPELLING: [(&str, &str); 14] = [
    ("file_tree", "GraphSource factory, not a step"),
    ("tinkerpop_modern", "GraphSource factory, not a step"),
    ("subgraph_labels", "schema map function, not a step"),
    ("has_p", "explicit Graphersal form of the overloaded has()"),
    (
        "has_value_p",
        "explicit Graphersal form of the overloaded hasValue()",
    ),
    ("has_value_any", "alias of hasValue(a, b, ...)"),
    (
        "where_p",
        "explicit Graphersal form of the overloaded where()",
    ),
    (
        "where_t",
        "explicit Graphersal form of the overloaded where()",
    ),
    ("profile_with", "Rust API name of profile(flags), an alias"),
    ("to_string", "Rhai's own conversion hook"),
    ("to_debug", "Rhai's own conversion hook"),
    ("typeOf", "type_of is a Rhai keyword"),
    ("toPlantUML", "third spelling of to_plantuml/toPlantuml"),
    (
        "localDateTime",
        "camelCase twin of the type name localdatetime (Java LocalDateTime)",
    ),
];

/// `has_label` -> `hasLabel`.
fn camel_case(name: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in name.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// `hasLabel` -> `has_label`.
fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn both_spellings_have_the_same_overloads() {
    let metadata: serde_json::Value = serde_json::from_str(
        &engine()
            .gen_fn_metadata_to_json(false)
            .unwrap_or_else(|err| panic!("metadata: {err}")),
    )
    .unwrap_or_else(|err| panic!("metadata json: {err}"));
    // name -> its overloads (parameter type lists)
    let mut overloads: std::collections::BTreeMap<String, BTreeSet<Vec<String>>> =
        Default::default();
    for f in metadata["functions"].as_array().into_iter().flatten() {
        let name = f["name"].as_str().unwrap_or_default();
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let params = f["params"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| p["type"].as_str().unwrap_or_default().to_string())
            .collect();
        overloads
            .entry(name.to_string())
            .or_default()
            .insert(params);
    }
    let mut problems = Vec::new();
    for (name, signatures) in &overloads {
        let starts_lower = name.chars().next().is_some_and(|c| c.is_ascii_lowercase());
        if name.ends_with('_') || !starts_lower {
            continue;
        }
        let other = if name.contains('_') {
            camel_case(name)
        } else if name.chars().any(|c| c.is_ascii_uppercase()) {
            snake_case(name)
        } else {
            continue;
        };
        if SINGLE_SPELLING.iter().any(|(single, _)| single == name) {
            continue;
        }
        match overloads.get(&other) {
            None => problems.push(format!("{name}: no `{other}`")),
            Some(others) if others != signatures => {
                problems.push(format!("{name}/{other}: different overloads"));
            }
            Some(_) => {}
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}
