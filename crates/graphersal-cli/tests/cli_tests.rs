//! Integration tests for the non-interactive `graphersal` query runner.
//!
//! Every test drives the compiled binary the way a shell script or an agent would: a script goes
//! in through stdin or `--eval`, and the assertions cover exactly what a caller can observe -
//! stdout, stderr and the exit code.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Output of one `graphersal` invocation, decoded as UTF-8.
struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

/// Runs the binary with `args`, feeding `stdin` to it, and captures everything it produced.
fn run(args: &[&str], stdin: &str) -> Run {
    let mut child = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("graphersal binary starts");

    child
        .stdin
        .as_mut()
        .expect("stdin is piped")
        .write_all(stdin.as_bytes())
        .expect("script is written to stdin");

    let output = child.wait_with_output().expect("graphersal terminates");
    Run {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        code: output.status.code().unwrap_or(-1),
    }
}

/// A unique scratch path for tests that write a graph file.
/// A path for a Rhai string literal: '/' separators (Windows accepts them; its '\\' would be
/// an escape in the literal).
fn script_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn temp_path(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("graphersal_cli_{}_{}", std::process::id(), name));
    path
}

#[test]
fn evaluates_script_from_stdin() {
    let res = run(
        &["--in", "--graph", "modern"],
        "g.v().values(\"age\").max().next();",
    );

    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "35");
    assert!(res.stderr.is_empty(), "stderr: {}", res.stderr);
}

#[test]
fn bare_pop_token_works_in_an_eval_script() {
    let res = run(
        &[
            "-e",
            r#"g.V("1").as("a").repeat(__.out().as("a")).times(2).select(first, "a").id().to_list()"#,
        ],
        "",
    );

    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(
        res.stdout.trim().lines().collect::<Vec<_>>(),
        ["\"1\"", "\"1\""]
    );
    assert!(res.stderr.is_empty(), "stderr: {}", res.stderr);
}

#[test]
fn qualified_pop_token_works_through_stdin() {
    let res = run(
        &["--in"],
        r#"g.V("1").as("a").repeat(__.out().as("a")).times(2).select(Pop.all, "a").by(__.unfold().id().fold()).to_list();"#,
    );

    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    let mut lines: Vec<_> = res.stdout.trim().lines().collect();
    lines.sort_unstable();
    assert_eq!(lines, [r#"["1", "4", "3"]"#, r#"["1", "4", "5"]"#]);
    assert!(res.stderr.is_empty(), "stderr: {}", res.stderr);
}

#[test]
fn piped_stdin_implies_batch_mode() {
    // No `--in`: a non-interactive stdin must not drop into the REPL.
    let res = run(&[], "g.v().count().next();");

    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "6");
}

#[test]
fn evaluates_positional_script() {
    let res = run(&["g.v().has_label(\"person\").count().next()"], "");

    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "4");
}

#[test]
fn evaluates_repeated_eval_flags_in_order() {
    let res = run(
        &["-e", "g.v().count().next()", "-e", "g.e().count().next()"],
        "",
    );

    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.lines().collect::<Vec<_>>(), vec!["6", "6"]);
}

#[test]
fn statements_share_one_scope() {
    let res = run(
        &[
            "-e",
            "let names = g.v().values(\"name\").to_list();",
            "-e",
            "names.len()",
        ],
        "",
    );

    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "6");
}

/// Type system step 4 acceptance: a `UUID.fromString(..)` literal survives `inject()` and
/// `next()` as a native `Uuid`, and a list of UUIDs renders in canonical form.
#[test]
fn uuid_literal_round_trips_as_a_native_uuid() {
    let res = run(
        &[
            "-e",
            r#"let x = g.inject(UUID.fromString("550e8400-e29b-41d4-a716-446655440000")).next()"#,
            "-e",
            "type_of(x)",
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "Uuid");

    let res = run(
        &[
            "-e",
            r#"g.inject(UUID::from_string("550e8400-e29b-41d4-a716-446655440000")).to_list()"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "550e8400-e29b-41d4-a716-446655440000");
}

/// Dates print as their canonical ISO 8601 text in tables and JSON, and as their DSL literal
/// where a script value is shown.
#[test]
fn dates_print_as_their_canonical_text_and_values_as_literals() {
    let query = r#"g.v(1).property("born", date("1990-05-01")).property("took", duration("P1M")).value_map("born", "took")"#;
    let table = run(&["-e", query], "");
    assert_eq!(table.code, 0, "stderr: {}", table.stderr);
    assert!(
        table.stdout.contains("│ [1990-05-01] │ [P1M] │"),
        "{}",
        table.stdout
    );
    let json = run(&["--format", "json", "-e", query], "");
    assert_eq!(json.code, 0, "stderr: {}", json.stderr);
    assert!(json.stdout.contains(r#""1990-05-01""#), "{}", json.stdout);
    let markdown = run(&["--format", "markdown", "-e", query], "");
    assert!(
        markdown.stdout.contains("| [1990-05-01] | [P1M] |"),
        "{}",
        markdown.stdout
    );
    let value = run(
        &[
            "-e",
            r#"[datetime("2023-08-02T09:30+02:00"), time("09:30")]"#,
        ],
        "",
    );
    assert_eq!(value.code, 0, "stderr: {}", value.stderr);
    assert_eq!(
        value.stdout.trim(),
        "datetime(\"2023-08-02T09:30:00+02:00\")\ntime(\"09:30:00\")"
    );
}

#[test]
fn vectors_print_their_kind_in_tables_and_as_literals_as_values() {
    let query =
        r#"g.v(1).property("emb", vector([0.5, -1.0, 0.25, 2.0, 3.0], "i8")).values("emb")"#;
    let table = run(&["-e", query], "");
    assert_eq!(table.code, 0, "stderr: {}", table.stderr);
    assert!(table.stdout.contains("vector[5 i8] ["), "{}", table.stdout);
    let json = run(&["--format", "json", "-e", query], "");
    assert_eq!(json.code, 0, "stderr: {}", json.stderr);
    assert!(json.stdout.contains("3.0"), "{}", json.stdout);
    let value = run(&["-e", r#"vector([0.5, 1.0])"#], "");
    assert_eq!(value.code, 0, "stderr: {}", value.stderr);
    assert_eq!(value.stdout.trim(), "vector([0.5, 1.0])");
}

/// JSON output writes a non-finite float as GraphSON's typed double instead of failing.
#[test]
fn non_finite_floats_are_typed_doubles_in_json_output() {
    let query = "g.inject(0.0 / 0.0, 1.0 / 0.0, [-1.0 / 0.0])";
    let json = run(&["--format", "json", "-e", query], "");
    assert_eq!(json.code, 0, "stderr: {}", json.stderr);
    let value: serde_json::Value = serde_json::from_str(&json.stdout).expect("valid JSON");
    assert_eq!(
        value,
        serde_json::json!([
            {"@type": "g:Double", "@value": "NaN"},
            {"@type": "g:Double", "@value": "Infinity"},
            [{"@type": "g:Double", "@value": "-Infinity"}],
        ])
    );
    let text = run(&["-e", "g.inject(0.0 / 0.0).to_json()"], "");
    assert_eq!(text.code, 0, "stderr: {}", text.stderr);
    let value: serde_json::Value = serde_json::from_str(&text.stdout).expect("valid JSON");
    assert_eq!(
        value,
        serde_json::json!([{"@type": "g:Double", "@value": "NaN"}])
    );
}

#[test]
fn failing_query_reports_diagnostic_on_stderr() {
    let res = run(&["-e", "g.v().values(\"name\").sum().next()"], "");

    assert_eq!(res.code, 1);
    assert!(res.stdout.is_empty(), "stdout: {}", res.stdout);
    assert!(
        res.stderr.contains("Cast exception"),
        "stderr: {}",
        res.stderr
    );
    assert!(res.stderr.contains("Help:"), "stderr: {}", res.stderr);
}

/// `g.gql(..)` runs in the CLI: JSON keeps the written column order (`who` before `what`), the
/// table sorts its columns like `project()`, and a GQL syntax error exits 1 with its GQLSTATUS.
#[test]
fn gql_queries_run_in_the_cli() {
    let query = r#"g.gql("MATCH (p:person)-[:created]->(s) RETURN p.name AS who, s.name AS what ORDER BY who, what")"#;
    let json = run(&["--format", "json", "-e", query], "");
    assert_eq!(json.code, 0, "stderr: {}", json.stderr);
    let rows: serde_json::Value = serde_json::from_str(&json.stdout).expect("JSON rows");
    assert_eq!(
        rows,
        serde_json::json!([
            {"who": "josh", "what": "lop"},
            {"who": "josh", "what": "ripple"},
            {"who": "marko", "what": "lop"},
            {"who": "peter", "what": "lop"}
        ])
    );
    assert!(
        json.stdout.find("\"who\"") < json.stdout.find("\"what\""),
        "{}",
        json.stdout
    );
    let table = run(&["-e", query], "");
    assert_eq!(table.code, 0, "stderr: {}", table.stderr);
    assert!(
        table.stdout.contains("│ ripple │ josh  │"),
        "{}",
        table.stdout
    );
    let refused = run(&["-e", r#"g.gql("MATCH (p:person) RETURN")"#], "");
    assert_eq!(refused.code, 1, "stdout: {}", refused.stdout);
    assert!(refused.stdout.is_empty(), "stdout: {}", refused.stdout);
    assert!(refused.stderr.contains("GQLSTATUS"), "{}", refused.stderr);
}

/// The Gremlin console idiom for a map's keys: `unfold()` of a `group()` result into entries,
/// then `select(Column.keys)`, one name per traverser.
#[test]
fn group_keys_via_unfold_and_select_column() {
    let res = run(
        &[
            "-e",
            r#"g.V().group().by("name").by(__.out().count()).unfold().select(Column.keys).toList()"#,
        ],
        "",
    );

    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    let mut names: Vec<&str> = res.stdout.lines().map(|l| l.trim_matches('"')).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec!["josh", "lop", "marko", "peter", "ripple", "vadas"]
    );
}

/// A traversal left without a terminal executes while it is rendered; when that fails, the
/// diagnostic goes to stderr and the exit code is 1, exactly like a terminated query.
#[test]
fn failing_unterminated_traversal_exits_with_query_error() {
    let res = run(
        &[
            "-e",
            r#"g.v().group().by("name").by(__.out().count()).properties().key()"#,
        ],
        "",
    );

    assert_eq!(res.code, 1, "stdout: {}", res.stdout);
    assert!(res.stdout.is_empty(), "stdout: {}", res.stdout);
    assert!(
        res.stderr
            .contains("expected type [vertex or edge], got type 'object'"),
        "stderr: {}",
        res.stderr
    );
    assert!(
        res.stderr.contains("unfold().select(Column.keys)"),
        "stderr: {}",
        res.stderr
    );
}

#[test]
fn unknown_option_is_a_usage_error() {
    let res = run(&["--nope"], "");

    assert_eq!(res.code, 2);
    assert!(
        res.stderr.contains("Unknown option"),
        "stderr: {}",
        res.stderr
    );
}

#[test]
fn a_bad_ai_file_stops_the_server_before_it_starts() {
    // The --ai file is checked first, before the UI directory and the graph.
    let res = run(&["--ai", "ai.json"], "");
    assert_eq!(res.code, 2);
    assert!(res.stderr.contains("--ai needs --server"), "{}", res.stderr);

    let missing = temp_path("missing_ai.json");
    let missing = missing.to_str().expect("utf-8 path");
    let res = run(&["--server", "--port", "0", "--ai", missing], "");
    assert_eq!(res.code, 2);
    assert!(
        res.stderr.contains("cannot read the --ai file"),
        "{}",
        res.stderr
    );

    let file = temp_path("bad_ai.json");
    std::fs::write(
        &file,
        r#"{"provider": "openai", "model": "m", "api_key": "k", "readonly": true}"#,
    )
    .expect("write the --ai file");
    let path = file.to_str().expect("utf-8 path");
    let res = run(&["--server", "--port", "0", "--ai", path], "");
    let _ = std::fs::remove_file(&file);
    assert_eq!(res.code, 2);
    assert!(
        res.stderr.contains("did you mean \"read_only\"?"),
        "{}",
        res.stderr
    );
    assert!(!res.stdout.contains("http://"), "the server must not start");
}

#[test]
fn unknown_format_is_a_usage_error() {
    let res = run(&["--format", "yaml", "-e", "g.v().count().next()"], "");

    assert_eq!(res.code, 2);
    assert!(
        res.stderr.contains("Unknown format"),
        "stderr: {}",
        res.stderr
    );
}

#[test]
fn missing_graph_file_is_a_usage_error() {
    let res = run(
        &[
            "--graph",
            "./definitely_missing.graphml",
            "-e",
            "g.v().count().next()",
        ],
        "",
    );

    assert_eq!(res.code, 2);
    assert!(res.stdout.is_empty(), "stdout: {}", res.stdout);
    assert!(!res.stderr.is_empty());
}

#[test]
fn unsupported_graph_file_extension_is_a_usage_error() {
    let res = run(
        &["--graph", "./graph.csv", "-e", "g.v().count().next()"],
        "",
    );

    assert_eq!(res.code, 2);
    assert!(res.stdout.is_empty(), "stdout: {}", res.stdout);
    assert!(
        res.stderr.contains("Unsupported graph file format"),
        "stderr: {}",
        res.stderr
    );
    assert!(res.stderr.contains("GraphML"), "stderr: {}", res.stderr);
}

#[test]
fn loads_a_graph_from_disk() {
    let path = temp_path("modern.graphml");
    let path = script_path(&path);

    let exported = run(
        &[
            "--graph",
            "modern",
            "-e",
            &format!("g.export_graphml(\"{path}\")"),
        ],
        "",
    );
    assert_eq!(exported.code, 0, "stderr: {}", exported.stderr);

    let res = run(&["--graph", &path, "-e", "g.v().count().next()"], "");
    let _ = std::fs::remove_file(&path);

    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "6");
}

/// `--graph x.json` reads GraphSON; `export_graphson` writes it (`.json` and the line form).
#[test]
fn graphson_export_reads_back_through_graph() {
    for (name, call) in [
        ("modern_snake.json", "export_graphson"),
        ("modern_lines.jsonl", "export_graphson"),
    ] {
        let path = temp_path(name);
        let path = script_path(&path);
        let exported = run(
            &["--graph", "modern", "-e", &format!("g.{call}(\"{path}\")")],
            "",
        );
        assert_eq!(exported.code, 0, "stderr: {}", exported.stderr);

        let queries = [
            "g.v().element_map().to_list()",
            "g.e().element_map().to_list()",
            "g.e().values(\"weight\").to_list().map(|x| type_of(x))",
        ];
        for query in queries {
            let original = run(&["--graph", "modern", "-e", query], "");
            let loaded = run(&["--graph", &path, "-e", query], "");
            assert_eq!(loaded.code, 0, "stderr: {}", loaded.stderr);
            assert!(loaded.stderr.is_empty(), "stderr: {}", loaded.stderr);
            // GraphSON groups edges by vertex and label, so their iteration order may differ.
            let sorted = |text: &str| {
                let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
                lines.sort();
                lines
            };
            assert_eq!(sorted(&loaded.stdout), sorted(&original.stdout), "{query}");
        }
        let _ = std::fs::remove_file(&path);
    }
}

/// Data a GraphSON import skips is named in a note on stderr; the result stays on stdout.
#[test]
fn graphson_skips_are_noted_on_stderr() {
    let path = temp_path("dates.graphson");
    std::fs::write(
        &path,
        r#"{"id":"v","label":"x","properties":{"born":[{"value":{"@type":"gx:Year","@value":"2016"}}],"seen":[{"value":{"@type":"g:Date","@value":1}}],"name":[{"value":"n"}]}}"#,
    )
    .unwrap();
    let path = path.to_string_lossy().to_string();
    let res = run(
        &["--graph", &path, "-e", "g.v().values(\"name\").next()"],
        "",
    );
    // A date/time value Graphersal holds is imported, not skipped.
    let seen = run(
        &["--graph", &path, "-e", "g.v().values(\"seen\").next()"],
        "",
    );
    let _ = std::fs::remove_file(&path);
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "n");
    assert!(
        res.stderr
            .contains("skipped date/time values (gx:Year, a type Graphersal cannot hold): 1"),
        "stderr: {}",
        res.stderr
    );
    assert!(!res.stderr.contains("g:Date"), "stderr: {}", res.stderr);
    assert_eq!(seen.code, 0, "stderr: {}", seen.stderr);
    assert_eq!(
        seen.stdout.trim(),
        r#"datetime("1970-01-01T00:00:00.001Z")"#
    );
}

/// A malformed GraphSON file is a usage error that names the line.
#[test]
fn malformed_graphson_is_a_usage_error() {
    let path = temp_path("broken.json");
    std::fs::write(&path, "{\"id\":\"a\"}\n{\"id\":\n").unwrap();
    let path = path.to_string_lossy().to_string();
    let res = run(&["--graph", &path, "-e", "g.v().count().next()"], "");
    let _ = std::fs::remove_file(&path);
    assert_eq!(res.code, 2);
    assert!(res.stdout.is_empty(), "stdout: {}", res.stdout);
    assert!(
        res.stderr.contains("GraphSON parse error at line 2"),
        "stderr: {}",
        res.stderr
    );
}

/// Exporting the modern graph and reading the file back reproduces every element exactly,
/// including value types (an integral `float64` weight stays a float, ids stay the same).
#[test]
fn graphml_export_of_modern_reads_back_unchanged() {
    let path = temp_path("modern_roundtrip.graphml");
    let path = script_path(&path);

    let exported = run(
        &[
            "--graph",
            "modern",
            "-e",
            &format!("g.export_graphml(\"{path}\")"),
        ],
        "",
    );
    assert_eq!(exported.code, 0, "stderr: {}", exported.stderr);

    let queries = [
        "g.v().element_map().to_list()",
        "g.e().element_map().to_list()",
        "g.e().values(\"weight\").to_list().map(|x| type_of(x))",
        "g.v().values(\"age\").to_list().map(|x| type_of(x))",
    ];
    for query in queries {
        let original = run(&["--graph", "modern", "-e", query], "");
        let reread = run(&["--graph", &path, "-e", query], "");
        assert_eq!(original.code, 0, "stderr: {}", original.stderr);
        assert_eq!(reread.code, 0, "stderr: {}", reread.stderr);
        assert_eq!(
            reread.stdout, original.stdout,
            "`{query}` differs after the round trip"
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn empty_graph_accepts_mutations() {
    let res = run(
        &[
            "--graph",
            "empty",
            "-e",
            "g.add_v(\"person\").property(\"name\", \"bob\").next();",
            "-e",
            "g.v().count().next()",
        ],
        "",
    );

    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    // The added vertex is echoed by `next()` (as `v[<id>]`), the count by the second script.
    assert!(res.stdout.contains("v["), "stdout: {}", res.stdout);
    assert_eq!(res.stdout.lines().last(), Some("1"));
}

/// T1b of the memory-inspection spec: `g.memory_usage()` is exercised end-to-end through `graphersal`,
/// this project's primary Rhai-to-Rust-API testing path (CLAUDE.md), and the output actually
/// contains the expected field names/values through the session visualizer, not just "compiles."
#[test]
fn memory_usage_reports_fields_through_the_visualizer() {
    let res = run(&["--graph", "modern", "-e", "g.memory_usage()"], "");

    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    for field in [
        "structure_bytes",
        "index_bytes",
        "property_bytes",
        "total_bytes",
        "total_mb",
    ] {
        assert!(
            res.stdout.contains(field),
            "missing {field:?} in stdout: {}",
            res.stdout
        );
    }
}

/// `graphersal` builds with `alloc-tracking` on by default (D7): `.profile_with(ProfileType::Memory)`'s
/// `Mem` column and its `TOTAL: memory materialized:` row must actually appear, with exact (not
/// `~`-prefixed estimated) figures for a step that genuinely materializes data. A bare `.profile()`
/// no longer collects or renders memory data at all (`ProfileType` bitmask spec, D2/D4).
#[test]
fn profile_reports_tracked_memory_figures() {
    let res = run(
        &[
            "--graph",
            "modern",
            "-e",
            "g.v().group().by(\"label\").profile(ProfileType::Memory)",
        ],
        "",
    );

    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert!(res.stdout.contains("Mem"), "stdout: {}", res.stdout);
    assert!(
        res.stdout.contains("TOTAL: memory materialized:"),
        "stdout: {}",
        res.stdout
    );
    // `group()` materializes an aggregation buffer, so its row must show a non-zero, non-`~`
    // (tracked, not estimated) byte figure.
    assert!(
        res.stdout
            .lines()
            .any(|l| l.contains("group") && !l.contains("~") && !l.contains(" 0B")),
        "stdout: {}",
        res.stdout
    );
}

/// A bare `.profile()` (no `ProfileType` argument) shows no memory data whatsoever: no `Mem`
/// column, no `TOTAL: memory materialized:` row (`ProfileType` bitmask spec, D2/D4's opt-in
/// behavior change from the previous always-on-memory `.profile()`).
#[test]
fn bare_profile_reports_no_memory_figures() {
    let res = run(
        &[
            "--graph",
            "modern",
            "-e",
            "g.v().group().by(\"label\").profile()",
        ],
        "",
    );

    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert!(!res.stdout.contains("Mem"), "stdout: {}", res.stdout);
    assert!(
        !res.stdout.contains("TOTAL: memory materialized:"),
        "stdout: {}",
        res.stdout
    );
}

/// Richer-memory-diagnostics spec (D-testing), updated for the `ProfileType` bitmask spec: the
/// `Retained`/`Allocs` figures now render as real table columns (not an annotation line) under
/// `.profile_with(ProfileType::Memory)`, end to end under `graphersal`'s default (`alloc-tracking`-on)
/// build, and are self-consistent: `Allocs > 0` whenever the step's `Mem` figure is `> 0`.
#[test]
fn profile_reports_net_retained_and_alloc_count() {
    let res = run(
        &[
            "--graph",
            "modern",
            "-e",
            "g.v().group().by(\"label\").profile(ProfileType::Memory)",
        ],
        "",
    );

    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    let group_row = res
        .stdout
        .lines()
        .find(|l| l.trim_start().starts_with("group"))
        .unwrap_or_else(|| panic!("no group(...) row in stdout: {}", res.stdout));
    let fields: Vec<&str> = group_row.split_whitespace().collect();
    // Step Call In Out Time Mem Retained Allocs %Dur
    let allocs = fields[fields.len() - 2];
    assert_ne!(allocs, "0", "line: {group_row}");
    assert_ne!(allocs, "-", "line: {group_row}");
}

/// Parses a `format_bytes`-style figure (`"208B"`, `"26.00KB"`, `"26.00MB"`, optionally `~`-
/// prefixed) into a byte count. Panics on anything else, since a test fixture producing a
/// differently-shaped figure is itself a bug worth surfacing loudly.
fn parse_bytes_figure(figure: &str) -> f64 {
    let figure = figure.trim_start_matches('~');
    if let Some(mb) = figure.strip_suffix("MB") {
        mb.parse::<f64>().expect("MB figure parses") * 1024.0 * 1024.0
    } else if let Some(kb) = figure.strip_suffix("KB") {
        kb.parse::<f64>().expect("KB figure parses") * 1024.0
    } else if let Some(b) = figure.strip_suffix('B') {
        b.parse::<f64>().expect("B figure parses")
    } else {
        panic!("unrecognized byte figure: {figure:?}");
    }
}

/// GraphStepFlatMap buffer-reuse spec (D6/Testing): a high-fan-out expansion step (`out()`) run
/// under `alloc-tracking` must keep its gross tracked bytes-per-produced-item under a bounded
/// multiple of a bare `Traverser`'s footprint.
///
/// This is a regression guard for the buffer-ownership rewrite of `GraphStepFlatMap::process()`
/// (`crates/graphersal/src/traversal/step/mod.rs`): before that change, every `process()` call
/// allocated its own small `Vec` (one per input traverser), which on the bundled `large` fixture
/// measured about 639 bytes/item for `out()` (67.76MB gross / 111100 produced). After the change,
/// the same query measures about 245 bytes/item (26.00MB gross / 111100 produced) — writing
/// directly into the caller's shared, amortized-growth buffer instead. The bound below (400
/// bytes/item) sits comfortably above the post-fix figure and below the pre-fix one, so a
/// regression that reintroduces a per-call allocation trips this test.
#[test]
fn expansion_step_gross_bytes_stay_within_a_bounded_multiple_of_baseline_per_item_cost() {
    let res = run(
        &[
            "--graph",
            "large",
            "-e",
            "g.v().out().has_label(\"V_1_0\").count().profile(ProfileType::Memory)",
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);

    let out_row = res
        .stdout
        .lines()
        .find(|l| l.trim_start().starts_with("out()"))
        .unwrap_or_else(|| panic!("no out() row in stdout: {}", res.stdout));
    // Step Call In Out Time Mem Retained Allocs %Dur
    let fields: Vec<&str> = out_row.split_whitespace().collect();
    let out_count: f64 = fields[3]
        .parse()
        .unwrap_or_else(|_| panic!("could not read out()'s Out column: {out_row:?}"));
    assert!(out_count > 0.0, "out() produced nothing: {out_row:?}");

    // The `Mem` column is now the gross figure directly (the `[retained: .., allocs: .., gross:
    // ..]` annotation line this test used to read was dropped, D4: its data moved into real
    // `Mem`/`Retained`/`Allocs` columns).
    let gross_bytes = parse_bytes_figure(fields[5]);

    let bytes_per_item = gross_bytes / out_count;
    assert!(
        bytes_per_item < 400.0,
        "out() gross bytes/item too high: {bytes_per_item:.1} (gross {gross_bytes}, count {out_count}) - \
         expected < 400 B/item post buffer-reuse fix, see doc comment on this test"
    );
}

/// dedup() identity-key spec (2026-09-24-dedup-identity-key, D1/D3): `dedup()` over a stream of
/// `Vertex`/`Edge` traversers must key by their cheap, `Copy` identity handle rather than fully
/// materializing each element's property set into a fresh `IndexMap` just to get a hashable key.
///
/// Before the fix, `dedup()` allocated roughly once per input item (an `IndexMap` plus cloned
/// property values per traverser): on an earlier 110k-element audit dataset (not kept in the repository)
/// (6,268,802 items reaching `dedup()`) that was 25,075,226 allocator calls and ~22s. After the
/// fix, keying by `S::VertexId`/`S::EdgeId` directly needs only the accumulator's own amortized
/// growth reallocations, independent of how many items flow through — on the smaller bundled
/// `large` fixture (111,100 items reaching `dedup()`), that measures 3 allocator calls, in the
/// same ballpark as `out()`/`in()`'s own amortized-buffer growth (16 allocator calls each on this
/// fixture). The bound below (50 calls) is a generous multiple of that observed floor: nowhere
/// near the old ~1-alloc-per-item shape, so a regression that reintroduces per-item
/// materialization trips this test regardless of which fixture size it runs against.
#[test]
fn dedup_allocs_stay_a_bounded_constant_not_a_per_item_multiple() {
    let res = run(
        &[
            "--graph",
            "large",
            "-e",
            "g.v().out().in_().dedup().count().profile(ProfileType::Memory)",
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);

    let dedup_row = res
        .stdout
        .lines()
        .find(|l| l.trim_start().starts_with("dedup()"))
        .unwrap_or_else(|| panic!("no dedup() row in stdout: {}", res.stdout));
    // Step Call In Out Time Mem Retained Allocs %Dur
    let fields: Vec<&str> = dedup_row.split_whitespace().collect();
    let in_count: f64 = fields[2]
        .parse()
        .unwrap_or_else(|_| panic!("could not read dedup()'s In column: {dedup_row:?}"));
    assert!(
        in_count > 1000.0,
        "dedup() saw too few items to be a meaningful check: {dedup_row:?}"
    );

    let allocs: f64 = fields[7]
        .parse()
        .unwrap_or_else(|_| panic!("could not read dedup()'s Allocs column: {dedup_row:?}"));
    assert!(
        allocs < 50.0,
        "dedup() allocs too high: {allocs} for {in_count} items in - expected a small bounded \
         constant (amortized accumulator growth only), not a per-item multiple; see doc comment \
         on this test"
    );
}

/// String-allocation-churn spec (2026-09-24-string-allocation-churn-in-value-paths, D1): a string
/// `has()`/`has_value()`/`is()`/`where()` comparand must not heap-allocate on every traverser it is
/// tested against. Before the fix, `P::test()`'s `P::Eq` arm cloned the comparand's `PValue::String`
/// (then backed by `std::String`) on every call, and its `TraverserValue::VertexProperty` arm always
/// materialized an owned `String` via `Traverser::as_string()` just to compare-and-discard it — two
/// allocator calls per input item (`176,001` for `88,000` items on an earlier 110k-element
/// audit dataset, see the spec's D3 tables). After the fix, `PValue::String` is backed by a
/// `CompactString` (a stack copy on `.clone()` for short comparands like this one, not a heap
/// allocation), and the property lookup borrows via the new `Traverser::as_string_ref()` instead of
/// `Traverser::as_string()`, so the whole `has_p(...)` step needs only its own one-time setup
/// allocation, independent of how many items it tests. The bound below (10) is a generous multiple
/// of the observed post-fix floor (1), nowhere near the old ~2-per-item shape.
#[test]
fn has_p_string_eq_allocs_stay_a_bounded_constant_not_a_per_item_multiple() {
    let res = run(
        &[
            "--graph",
            "large",
            "-e",
            "g.v().has(\"level\", P::eq(\"0\")).count().profile(ProfileType::Memory)",
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);

    let has_p_row = res
        .stdout
        .lines()
        .find(|l| l.trim_start().starts_with("has(\"level\""))
        .unwrap_or_else(|| panic!("no has_p(...) row in stdout: {}", res.stdout));
    // Step Call In Out Time Mem Retained Allocs %Dur — indexed from the right, since the step
    // name itself (`has("level", P.eq("0"))`) contains whitespace that would otherwise
    // shift a left-relative index.
    let fields: Vec<&str> = has_p_row.split_whitespace().collect();
    let in_count: f64 = fields[fields.len() - 7]
        .parse()
        .unwrap_or_else(|_| panic!("could not read has_p(...)'s In column: {has_p_row:?}"));
    assert!(
        in_count > 1000.0,
        "has_p(...) saw too few items to be a meaningful check: {has_p_row:?}"
    );

    let allocs: f64 = fields[fields.len() - 2]
        .parse()
        .unwrap_or_else(|_| panic!("could not read has_p(...)'s Allocs column: {has_p_row:?}"));
    assert!(
        allocs < 10.0,
        "has_p(...) allocs too high: {allocs} for {in_count} items in - expected a small bounded \
         constant (no per-item comparand clone or owned-String materialization), not a per-item \
         multiple; see doc comment on this test"
    );
}

/// String-allocation-churn spec (D2): `values(key)` must not clone the property key into an owned
/// `String` and then convert it to `CompactString` (an allocate-then-immediately-drop round trip)
/// for every item it processes. Before the fix this was the other half of the same query's
/// 2-allocs/item shape measured above; after the fix `CompactString::new(key.as_str())` builds the
/// key without a heap round trip. The remaining ~1 alloc/item is a separate, out-of-scope cost (not
/// traced by this spec, see its Non-goals) — this test only guards against the fixed line
/// regressing back toward the old ~2-per-item shape, asserting a generous multiple (1.5x) of the
/// observed post-fix floor (~1x) rather than the pre-fix ~2x.
#[test]
fn values_key_allocs_do_not_regress_to_the_old_two_per_item_shape() {
    let res = run(
        &[
            "--graph",
            "large",
            "-e",
            "g.v().values(\"level\").count().profile(ProfileType::Memory)",
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);

    let values_row = res
        .stdout
        .lines()
        .find(|l| l.trim_start().starts_with("values("))
        .unwrap_or_else(|| panic!("no values(...) row in stdout: {}", res.stdout));
    // Step Call In Out Time Mem Retained Allocs %Dur
    let fields: Vec<&str> = values_row.split_whitespace().collect();
    let in_count: f64 = fields[2]
        .parse()
        .unwrap_or_else(|_| panic!("could not read values(...)'s In column: {values_row:?}"));
    assert!(
        in_count > 1000.0,
        "values(...) saw too few items to be a meaningful check: {values_row:?}"
    );

    let allocs: f64 = fields[fields.len() - 2]
        .parse()
        .unwrap_or_else(|_| panic!("could not read values(...)'s Allocs column: {values_row:?}"));
    assert!(
        allocs < in_count * 1.5,
        "values(...) allocs too high: {allocs} for {in_count} items in - expected roughly 1 \
         alloc/item post-fix, not the pre-fix ~2/item shape; see doc comment on this test"
    );
}

/// Writes a directory tree as GraphML to `path` and returns its vertex count: `depth` levels of
/// `directory` vertices with `fan_out` subdirectories each, and `files` `file` vertices in every
/// directory (extensions `.rs`, `.md`, `.txt` in turn). Every vertex has a `name`; edges are
/// `contains` (parent -> child).
fn write_tree_graphml(path: &std::path::Path, fan_out: usize, files: usize, depth: usize) -> usize {
    const EXTENSIONS: [&str; 3] = ["rs", "md", "txt"];
    let mut nodes = String::new();
    let mut edges = String::new();
    let mut count = 1;
    nodes.push_str(
        r#"<node id="d"><data key="labelV">directory</data><data key="name">d</data></node>"#,
    );
    let mut level = vec![String::from("d")];
    for d in 0..depth {
        let mut next = Vec::with_capacity(level.len() * fan_out);
        for parent in &level {
            for f in 0..files {
                let name = format!("f{f}.{}", EXTENSIONS[f % EXTENSIONS.len()]);
                let id = format!("{parent}/{name}");
                nodes.push_str(&format!(
                    r#"<node id="{id}"><data key="labelV">file</data><data key="name">{name}</data></node>"#
                ));
                edges.push_str(&format!(
                    r#"<edge id="c:{id}" source="{parent}" target="{id}"><data key="labelE">contains</data></edge>"#
                ));
                count += 1;
            }
            if d + 1 == depth {
                continue;
            }
            for s in 0..fan_out {
                let name = format!("d{s}");
                let id = format!("{parent}/{name}");
                nodes.push_str(&format!(
                    r#"<node id="{id}"><data key="labelV">directory</data><data key="name">{name}</data></node>"#
                ));
                edges.push_str(&format!(
                    r#"<edge id="c:{id}" source="{parent}" target="{id}"><data key="labelE">contains</data></edge>"#
                ));
                count += 1;
                next.push(id);
            }
        }
        level = next;
    }
    let xml = format!(
        r#"<graphml xmlns="http://graphml.graphdrawing.org/xmlns"><key id="labelV" for="node" attr.name="labelV" attr.type="string"/><key id="name" for="node" attr.name="name" attr.type="string"/><key id="labelE" for="edge" attr.name="labelE" attr.type="string"/><graph id="G" edgedefault="directed">{nodes}{edges}</graph></graphml>"#
    );
    std::fs::write(path, xml).expect("tree GraphML is written");
    count
}

/// A profile `Mem`/`Retained` cell (`668B`, `320.57KB`, `5.00MB`) in bytes.
fn parse_profile_bytes(cell: &str) -> f64 {
    let cell = cell.trim_start_matches('~');
    let (number, scale) = if let Some(n) = cell.strip_suffix("MB") {
        (n, 1024.0 * 1024.0)
    } else if let Some(n) = cell.strip_suffix("KB") {
        (n, 1024.0)
    } else if let Some(n) = cell.strip_suffix('B') {
        (n, 1.0)
    } else {
        panic!("not a byte figure: {cell:?}")
    };
    number
        .parse::<f64>()
        .unwrap_or_else(|_| panic!("not a byte figure: {cell:?}"))
        * scale
}

/// The `glob_path(...)` row of a memory profile of the glob_path allocation-guard query.
struct GlobPathRow {
    out: f64,
    mem: f64,
    allocs: f64,
    stdout: String,
}

/// The glob_path allocation-guard query of step 5.
const GLOB_PATH_GUARD_QUERY: &str = r#"g.v("d").glob_path("**/*.rs").by("name").by(["directory","file"]).count().profile(ProfileType::Memory)"#;

/// The same query bounded by `max_depth` (glob_path spec, step 8): 6 levels, so the walk of the
/// 5-level tree is complete and the one of the 7-level tree is cut off.
const GLOB_PATH_GUARD_MAX_DEPTH_QUERY: &str = r#"g.v("d").glob_path("**/*.rs", #{max_depth: 6}).by("name").by(["directory","file"]).count().profile(ProfileType::Memory)"#;

/// Runs a glob_path allocation-guard `query` against the GraphML tree at `path`.
fn glob_path_count_allocs(path: &std::path::Path, query: &str) -> GlobPathRow {
    let path = path.to_string_lossy();
    let res = run(&["--graph", &path, "-e", query], "");
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    let row = res
        .stdout
        .lines()
        .find(|l| l.trim_start().starts_with("glob_path("))
        .unwrap_or_else(|| panic!("no glob_path(...) row in stdout: {}", res.stdout));
    // Step Call In Out Time Mem Retained Allocs %Dur, indexed from the right.
    let fields: Vec<&str> = row.split_whitespace().collect();
    let out: f64 = fields[fields.len() - 6]
        .parse()
        .unwrap_or_else(|_| panic!("could not read glob_path(...)'s Out column: {row:?}"));
    let mem = parse_profile_bytes(fields[fields.len() - 4]);
    let allocs: f64 = fields[fields.len() - 2]
        .parse()
        .unwrap_or_else(|_| panic!("could not read glob_path(...)'s Allocs column: {row:?}"));
    GlobPathRow {
        out,
        mem,
        allocs,
        stdout: res.stdout.clone(),
    }
}

/// glob_path spec, step 5: `glob_path(..).by(..).count()` counts through the step's own `count()`
/// hook, which never creates a traverser and, on a tree, never touches the `visited` map or the
/// dedup set. What it allocates is the reusable DFS stack's growth (bounded by depth x fan-out)
/// and a few per-execution setups, so neither the allocator calls nor the bytes grow with the
/// size of the tree. Measured on two trees of this shape (the bounds were set on trees 5x larger, 2,730 and 43,690 matches): 6 allocator
/// calls and 668 B gross on both. Materializing the same matches (`...by(..).dedup().count()`)
/// costs 320.57 KB (16 calls) and 5.00 MB (20 calls), so the byte bound is what catches a fall
/// back to materializing; the call bound catches per-vertex allocation. Both bounds are a
/// generous margin over the observed floor. The same bounds hold with `max_depth` (step 8), whose
/// depth bookkeeping only runs for the start and vertices with in-degree >= 2, which a tree has
/// none of: measured 7 allocator calls and 684 B on both trees (2,730 and 10,922 matches).
#[test]
fn glob_path_count_allocs_do_not_grow_with_the_tree() {
    let small_path = temp_path("glob_tree_small.graphml");
    let large_path = temp_path("glob_tree_large.graphml");
    let small_vertices = write_tree_graphml(&small_path, 3, 6, 5);
    let large_vertices = write_tree_graphml(&large_path, 3, 6, 7);
    assert!(
        small_vertices >= 800 && large_vertices >= 7_000,
        "{small_vertices} / {large_vertices}"
    );

    let small = glob_path_count_allocs(&small_path, GLOB_PATH_GUARD_QUERY);
    let large = glob_path_count_allocs(&large_path, GLOB_PATH_GUARD_QUERY);
    let bounded_small = glob_path_count_allocs(&small_path, GLOB_PATH_GUARD_MAX_DEPTH_QUERY);
    let bounded_large = glob_path_count_allocs(&large_path, GLOB_PATH_GUARD_MAX_DEPTH_QUERY);
    let _ = std::fs::remove_file(&small_path);
    let _ = std::fs::remove_file(&large_path);
    assert!(
        small.out > 200.0 && large.out > 5.0 * small.out,
        "{}\n{}",
        small.stdout,
        large.stdout
    );
    assert_allocs_do_not_grow(small_vertices, large_vertices, &small, &large);
    // The bound cuts the larger tree's walk, and the result still grows with the tree.
    assert_eq!(bounded_small.out, small.out, "{}", bounded_small.stdout);
    assert!(
        bounded_large.out > 1.5 * bounded_small.out && bounded_large.out < large.out,
        "{}",
        bounded_large.stdout
    );
    assert_allocs_do_not_grow(
        small_vertices,
        large_vertices,
        &bounded_small,
        &bounded_large,
    );
}

/// Asserts the allocation bounds of `glob_path_count_allocs_do_not_grow_with_the_tree` on the
/// profile rows of the small and the large tree.
fn assert_allocs_do_not_grow(
    small_vertices: usize,
    large_vertices: usize,
    small: &GlobPathRow,
    large: &GlobPathRow,
) {
    let both = format!("{}\n{}", small.stdout, large.stdout);
    assert!(
        small.allocs < 32.0 && large.allocs < 32.0 && large.allocs <= small.allocs + 8.0,
        "glob_path(..).count() allocator calls grow with the tree: {} for {small_vertices} \
         vertices, {} for {large_vertices} vertices - expected a small constant; see the doc \
         comment on this test\n{both}",
        small.allocs,
        large.allocs,
    );
    assert!(
        small.mem < 16.0 * 1024.0 && large.mem < 16.0 * 1024.0,
        "glob_path(..).count() allocates bytes in proportion to its matches: {} B for {} matches, \
         {} B for {} matches - expected a small constant; see the doc comment on this test\n{both}",
        small.mem,
        small.out,
        large.mem,
        large.out,
    );
}

#[test]
fn format_flag_selects_the_visualizer() {
    let res = run(&["--format", "mermaid", "-e", "g.infer_schema()"], "");

    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert!(res.stdout.contains("erDiagram"), "stdout: {}", res.stdout);
}

/// A schema as JSON is its canonical format (JSON Schema type names inside the envelope), the
/// same text `set_schema` reads back.
#[test]
fn json_schema_output_is_the_schema_format() {
    let res = run(&["--format", "json", "-e", "g.infer_schema()"], "");

    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    for expected in [
        r#""$schema": "https://graphersal.dev/schema/v1""#,
        r#""type": "string""#,
        r#""type": "integer""#,
        r#""type": "number""#,
        r#""required": ["#,
    ] {
        assert!(
            res.stdout.contains(expected),
            "missing {expected} in stdout: {}",
            res.stdout
        );
    }
    assert!(!res.stdout.contains("int64"), "stdout: {}", res.stdout);
    assert!(!res.stdout.contains("null"), "stdout: {}", res.stdout);
}

/// The schema methods from the command line: set from JSON text, read back, enforced.
#[test]
fn schema_methods_from_the_command_line() {
    let res = run(
        &[
            "--graph",
            "empty",
            "-e",
            r#"g.set_schema(`{"mode": "closed", "vertices": {"p": {"schema": {"type": "object",
                "properties": {"n": {"type": "string"}}, "required": ["n"]}}}}`)"#,
            "-e",
            r#"g.add_v("p").property("n", "a").count().next()"#,
            "-e",
            "g.get_schema().mode",
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert!(
        res.stdout.trim_end().ends_with("closed"),
        "stdout: {}",
        res.stdout
    );
    let res = run(
        &[
            "--graph",
            "empty",
            "-e",
            r#"g.set_schema(`{"mode": "closed", "vertices": {"p": {}}}`)"#,
            "-e",
            r#"g.add_v("q").next()"#,
        ],
        "",
    );
    assert_eq!(res.code, 1, "stdout: {}", res.stdout);
    assert!(
        res.stderr.contains("not declared in the closed schema"),
        "stderr: {}",
        res.stderr
    );
}

/// `--format json` renders values as plain JSON through the physical codec: no Rust enum
/// wrappers, and a UUID as its canonical string.
#[test]
fn json_format_renders_plain_json_values() {
    let res = run(
        &[
            "--format",
            "json",
            "-e",
            r#"g.v().has_label("person").limit(1).value_map()"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    let compact: String = res.stdout.chars().filter(|c| !c.is_whitespace()).collect();
    // valueMap() wraps every value in a list, as in TinkerPop.
    assert_eq!(compact, r#"[{"name":["marko"],"age":[29]}]"#);

    let res = run(
        &[
            "--format",
            "json",
            "-e",
            r#"g.v(1).property("uid", UUID.fromString("123e4567-e89b-12d3-a456-426614174000")).element_map()"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert!(
        res.stdout
            .contains(r#""uid": "123e4567-e89b-12d3-a456-426614174000""#),
        "stdout: {}",
        res.stdout
    );
    for wrapper in [
        "\"Object\"",
        "\"String\"",
        "\"Number\"",
        "\"NegInt\"",
        "\"Uuid\"",
    ] {
        assert!(
            !res.stdout.contains(wrapper),
            "{wrapper} in stdout: {}",
            res.stdout
        );
    }
}

/// The REPL's `/` commands, driven through `--repl` (the shell on a piped stdin): `/help` lists
/// the commands, `/help <name>` prints the engine's doc in either spelling, an unknown name and
/// the removed `:` prefix each get one pointer, and statements still run.
#[test]
fn repl_slash_commands_print_help_from_the_engine_docs() {
    let help = |input: &str| run(&["--repl"], &format!("{input}\n")).stdout;
    let list = help("/help");
    for command in [
        "/help <name>",
        "/help steps",
        "/set max-rows <n>",
        "/set spelling",
        "/exit",
    ] {
        assert!(list.contains(command), "{command}: {list}");
    }
    let snake = help("/help has_label");
    assert!(snake.contains("has_label  (step"), "{snake}");
    assert!(snake.contains("also: hasLabel"), "{snake}");
    assert!(snake.contains("Keeps elements whose label"), "{snake}");
    assert!(snake.contains("Example: `"), "{snake}");
    let camel = help("/help hasLabel");
    assert!(camel.contains("Keeps elements whose label") && camel.contains("Example: `"));
    // Under `/set spelling camel` the heading and signatures use the Gremlin spelling.
    let respelled = help("/set spelling camel\n/help has_label");
    assert!(respelled.contains("hasLabel  (step"), "{respelled}");
    let unknown = help("/help nosuchstep");
    assert!(unknown.contains("No help for `nosuchstep`"), "{unknown}");
    let old = run(&["--repl"], ":help\n:set max-rows 5\n");
    assert!(
        old.stdout.contains("Commands start with `/`: use `/help`"),
        "{}",
        old.stdout
    );
    assert!(
        old.stdout.contains("use `/set max-rows 5`"),
        "{}",
        old.stdout
    );
    assert_eq!(old.code, 0);
    let script = run(&["--repl"], "g.v().count().next();\n/bogus\n");
    assert!(script.stdout.contains("\n6\n"), "{}", script.stdout);
    assert!(
        script.stdout.contains("Unknown command: /bogus"),
        "{}",
        script.stdout
    );
    // `--repl` is the shell: it refuses a script to evaluate.
    assert_eq!(run(&["--repl", "-e", "1"], "").code, 2);
}

#[test]
fn help_and_version_exit_cleanly() {
    let help = run(&["--help"], "");
    assert_eq!(help.code, 0);
    assert!(help.stdout.contains("USAGE:"), "stdout: {}", help.stdout);
    assert!(
        help.stdout.contains("--timeout <MS>"),
        "stdout: {}",
        help.stdout
    );

    let version = run(&["--version"], "");
    assert_eq!(version.code, 0);
    assert!(
        version.stdout.starts_with("graphersal "),
        "stdout: {}",
        version.stdout
    );
}

#[test]
fn graph_tree_loads_the_file_tree_sample() {
    for spec in ["tree", "file_tree", "file-tree", "TREE"] {
        let res = run(&["--graph", spec, "-e", "g.v().count().next()"], "");
        assert_eq!(res.code, 0, "--graph {spec}, stderr: {}", res.stderr);
        assert_eq!(res.stdout.trim(), "37", "--graph {spec}");
    }

    let res = run(
        &[
            "--graph",
            "tree",
            "-e",
            "g.v(\"root/author/x/some/f.rs\").values(\"name\").next()",
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "f.rs");

    let help = run(&["--help"], "");
    assert!(help.stdout.contains("\"tree\""), "stdout: {}", help.stdout);
}

#[test]
fn timeout_flag_parses_and_leaves_fast_queries_alone() {
    let res = run(&["--timeout", "60000", "-e", "g.v().count().next()"], "");
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "6");

    let res = run(&["--timeout=0", "-e", "g.v().count().next()"], "");
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "6");
}

#[test]
fn timing_out_query_exits_with_the_diagnostic_on_stderr() {
    let res = run(
        &[
            "--graph",
            "large",
            "--timeout",
            "1",
            "-e",
            "g.V().out().out().out().count().next()",
        ],
        "",
    );
    assert_eq!(res.code, 1, "stderr: {}", res.stderr);
    assert!(res.stdout.is_empty(), "stdout: {}", res.stdout);
    assert!(
        res.stderr
            .contains("Traversal exceeded evaluationTimeout of 1 ms"),
        "stderr: {}",
        res.stderr
    );
    assert!(
        res.stderr
            .contains("Help: The traversal ran longer than its evaluationTimeout"),
        "stderr: {}",
        res.stderr
    );
}

#[test]
fn a_query_overrides_the_default_timeout() {
    let res = run(
        &[
            "--graph",
            "large",
            "--timeout",
            "1",
            "-e",
            "g.with(\"evaluationTimeout\", 0).V().out().out().out().count().next()",
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "110000");
}

#[test]
fn a_script_loop_cannot_bypass_the_timeout() {
    let res = run(&["--timeout", "50", "-e", "loop {}"], "");
    assert_eq!(res.code, 1, "stderr: {}", res.stderr);
    assert!(
        res.stderr
            .contains("Script exceeded evaluationTimeout of 50 ms"),
        "stderr: {}",
        res.stderr
    );
}

#[test]
fn invalid_timeout_is_a_usage_error() {
    for value in ["-1", "abc", "1.5"] {
        let res = run(&["--timeout", value, "-e", "g.v().count().next()"], "");
        assert_eq!(res.code, 2, "value {value}, stderr: {}", res.stderr);
        assert!(
            res.stderr.contains("Invalid --timeout value"),
            "stderr: {}",
            res.stderr
        );
    }
}

#[test]
fn repeat_acceptance_queries() {
    let res = run(
        &[
            "-e",
            r#"g.V("1").repeat(__.out()).until(__.has("name","ripple")).path().by("name").to_list()"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), r#"["marko", "josh", "ripple"]"#);

    let res = run(
        &[
            "-e",
            r#"g.V("1").emit().repeat(__.out("knows")).times(2).values("name").to_list()"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "\"marko\"\n\"josh\"\n\"vadas\"");
}

#[test]
fn a_runaway_repeat_exits_with_the_loop_limit_diagnostic() {
    let res = run(
        &[
            "-e",
            r#"g.with("repeat.max_loops", 20).V("1").repeat(__.both()).to_list()"#,
        ],
        "",
    );
    assert_eq!(res.code, 1);
    assert!(res.stdout.is_empty(), "stdout: {}", res.stdout);
    assert!(
        res.stderr
            .contains("repeat() exceeded the loop limit of 20 iterations"),
        "stderr: {}",
        res.stderr
    );
    assert!(
        res.stderr.contains("repeat(__.both().simple_path())"),
        "stderr: {}",
        res.stderr
    );
}

/// Rows of a rounded table: every line that starts with a cell border, minus the header.
fn table_rows(table: &str) -> usize {
    table
        .lines()
        .filter(|line| line.starts_with('│'))
        .count()
        .saturating_sub(1)
}

const ROWS_NOTICE: &str = "… showing the first 100 rows; more exist.";

#[test]
fn a_large_result_is_cut_to_100_rows_with_the_notice_on_stderr() {
    let res = run(&["--graph", "large", "-e", "g.v()"], "");
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(table_rows(&res.stdout), 100);
    assert!(!res.stdout.contains('…'), "the notice never goes to stdout");
    assert!(
        res.stderr.starts_with(ROWS_NOTICE),
        "stderr: {}",
        res.stderr
    );
    assert!(res.stderr.contains("--no-limit"), "stderr: {}", res.stderr);
}

#[test]
fn no_limit_prints_everything() {
    let res = run(&["--graph", "large", "--no-limit", "-e", "g.v()"], "");
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(table_rows(&res.stdout), 111_110);
    assert!(res.stderr.is_empty(), "stderr: {}", res.stderr);
}

#[test]
fn an_explicit_table_is_bounded_unless_the_call_says_no_limit() {
    let res = run(&["--graph", "large", "-e", "g.v().to_table()"], "");
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(table_rows(&res.stdout), 100);
    assert!(
        res.stderr.starts_with(ROWS_NOTICE),
        "stderr: {}",
        res.stderr
    );

    let res = run(
        &[
            "--graph",
            "large",
            "-e",
            "g.v().to_table(#{no_limit: true})",
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(table_rows(&res.stdout), 111_110);
    assert!(res.stderr.is_empty(), "stderr: {}", res.stderr);
}

#[test]
fn json_output_stays_parseable_when_cut() {
    let res = run(&["--graph", "large", "--format", "json", "-e", "g.v()"], "");
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    let json: serde_json::Value = serde_json::from_str(&res.stdout).expect("stdout is JSON");
    assert_eq!(json.as_array().expect("an array").len(), 100);
    assert!(
        res.stderr.starts_with(ROWS_NOTICE),
        "stderr: {}",
        res.stderr
    );
}

#[test]
fn a_final_list_is_cut_the_same_way() {
    // 150 items are enough to pass the 100-row limit; the large fixture is not needed.
    let res = run(
        &[
            "--graph",
            "empty",
            "-e",
            "let xs = []; for i in 0..150 { xs.push(i); } xs",
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.lines().count(), 100);
    assert!(
        res.stderr.starts_with(ROWS_NOTICE),
        "stderr: {}",
        res.stderr
    );
}

#[test]
fn data_in_variables_is_never_cut() {
    let res = run(
        &["--graph", "large", "-e", "let x = g.v().to_list(); x.len()"],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "111110");
    assert!(res.stderr.is_empty(), "stderr: {}", res.stderr);
}

#[test]
fn display_limit_flags_set_the_limits() {
    let res = run(&["--max-rows", "3", "-e", "g.v()"], "");
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(table_rows(&res.stdout), 3);
    assert!(
        res.stderr
            .starts_with("… showing the first 3 rows; more exist."),
        "stderr: {}",
        res.stderr
    );

    let res = run(
        &["--max-items", "2", "-e", r#"g.v().values("name").fold()"#],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert!(
        res.stdout.contains("[marko, vadas, …]"),
        "stdout: {}",
        res.stdout
    );
    assert!(
        res.stderr
            .starts_with("… nested collections were cut to 2 items."),
        "stderr: {}",
        res.stderr
    );
}

#[test]
fn no_limit_with_another_limit_is_an_invalid_invocation() {
    let res = run(&["--no-limit", "--max-rows", "5", "-e", "g.v()"], "");
    assert_eq!(res.code, 2);
    assert!(res.stdout.is_empty());
    assert!(res.stderr.contains("--no-limit"), "stderr: {}", res.stderr);

    let res = run(&["--max-items", "0", "-e", "g.v()"], "");
    assert_eq!(res.code, 2);
}

/// A `jpath(..)` value prints its canonical path text, not the Rust type name, alone and nested.
#[test]
fn jpath_value_renders_as_canonical_text() {
    let single = run(&["-e", r#"jpath("a.b[1].c")"#], "");
    assert_eq!(single.code, 0, "{}", single.stderr);
    assert_eq!(single.stdout.trim(), "$.a.b[1].c");

    let list = run(&["-e", r#"[jpath("a"), jpath("['x y'][-1]")]"#], "");
    assert_eq!(list.code, 0, "{}", list.stderr);
    assert!(list.stdout.contains("$.a"), "{}", list.stdout);
    assert!(list.stdout.contains("$['x y'][-1]"), "{}", list.stdout);
    assert!(!list.stdout.contains("graphersal::"), "{}", list.stdout);
}

/// A DSL token prints as its literal (A07): as the result, nested in a list, through
/// `to_string()` and in `${..}`; an anonymous traversal prints its text in the session spelling.
#[test]
fn tokens_render_as_their_dsl_literal() {
    for (script, expected) in [
        ("Scope.local", "Scope.local"),
        ("Order::Desc", "Order.desc"),
        ("T.id", "T.id"),
        ("Operator.sum", "Operator.sum"),
        ("Pick.any", "Pick.any"),
        ("GType.LONG", "GType.LONG"),
        ("SchemaMode.closed", "SchemaMode.closed"),
        ("ProfileType.Memory", "ProfileType.Memory"),
        ("keys", "Column.keys"),
        ("P.gt(1).and(P.lt(3))", "P.gt(1).and(P.lt(3))"),
        (r#"TextP.containing("a")"#, r#"TextP.containing("a")"#),
        ("P.gt(1).to_string()", "P.gt(1)"),
        (
            "`${Barrier.normSack} ${first}`",
            "Barrier.normSack Pop.first",
        ),
        (
            r#"__.out().has("name", "x")"#,
            r#"__.out().has("name", "x")"#,
        ),
        ("[Scope.local, Column.values]", "Scope.local\nColumn.values"),
    ] {
        let res = run(&["-e", script], "");
        assert_eq!(res.code, 0, "{script}: {}", res.stderr);
        assert_eq!(res.stdout.trim(), expected, "{script}");
    }

    let camel = run(
        &["--spelling", "camel", "-e", r#"__.out_e().values("name")"#],
        "",
    );
    assert_eq!(camel.code, 0, "{}", camel.stderr);
    assert_eq!(camel.stdout.trim(), r#"__.outE().values("name")"#);
}

/// End to end on the modern graph: a nested property written by `property_json` is read, filtered
/// and projected through `jpath(..)` keys, and a missing path is simply absent.
#[test]
fn jpath_keys_read_nested_properties() {
    let setup = r#"g.V("1").property_json(#{meta: #{tags: ["x", "y"], k: 1}}).toList()"#;
    let r = run(
        &[
            "-e",
            setup,
            "-e",
            r#"g.v().has(jpath("meta.tags[0]"), "x").values(jpath("meta.tags[0]")).to_list()"#,
        ],
        "",
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.stdout.lines().last(), Some(r#""x""#), "{}", r.stdout);

    let none = run(
        &[
            "-e",
            setup,
            "-e",
            r#"g.v().values(jpath("meta.nope")).to_list()"#,
        ],
        "",
    );
    assert_eq!(none.code, 0, "{}", none.stderr);
    assert!(none.stderr.is_empty(), "{}", none.stderr);
    assert!(!none.stdout.contains("nope"), "{}", none.stdout);
}

#[test]
fn scripts_run_without_resource_limits_by_default() {
    // 2^26 bytes is above the SAFE string limit (16 MiB) and still instant without limits.
    let res = run(
        &["-e", r#"let s = "x"; for i in 0..26 { s += s; } s.len()"#],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "67108864");
}

#[test]
fn safe_limits_stop_the_same_script() {
    let res = run(
        &[
            "--safe-limits",
            "-e",
            r#"let s = "x"; for i in 0..26 { s += s; } s.len()"#,
        ],
        "",
    );
    assert_eq!(res.code, 1);
    assert!(res.stdout.is_empty(), "stdout: {}", res.stdout);
    assert!(
        res.stderr
            .contains("Resource limit 'script.max_string_size' exceeded"),
        "{}",
        res.stderr
    );
}

#[test]
fn max_operations_stops_an_infinite_loop_with_a_help_that_names_the_flag() {
    let res = run(
        &[
            "--max-operations",
            "100000",
            "-e",
            "let n = 0; loop { n += 1; }",
        ],
        "",
    );
    assert_eq!(res.code, 1);
    assert!(res.stdout.is_empty(), "stdout: {}", res.stdout);
    assert!(
        res.stderr
            .contains("Resource limit 'script.max_operations' exceeded (limit 100000)"),
        "{}",
        res.stderr
    );
    assert!(res.stderr.contains("--max-operations"), "{}", res.stderr);
}

#[test]
fn max_string_and_map_flags_apply() {
    let res = run(
        &[
            "--max-string-size",
            "1000",
            "-e",
            r#"let s = "x"; loop { s += s; }"#,
        ],
        "",
    );
    assert_eq!(res.code, 1);
    assert!(
        res.stderr.contains("script.max_string_size"),
        "{}",
        res.stderr
    );
    let res = run(
        &[
            "--max-map-size",
            "50",
            "-e",
            "let m = #{}; let i = 0; loop { let o = #{}; o[`k${i}`] = i; m.mixin(o); i += 1; }",
        ],
        "",
    );
    assert_eq!(res.code, 1);
    assert!(res.stderr.contains("script.max_map_size"), "{}", res.stderr);
}

#[test]
fn max_traversers_caps_a_traversal() {
    let res = run(&["--max-traversers", "3", "-e", "g.v().to_list()"], "");
    assert_eq!(res.code, 1);
    assert!(
        res.stderr
            .contains("Resource limit 'traversal.max_traversers' exceeded (limit 3, reached 6)"),
        "{}",
        res.stderr
    );
    // 0 means unlimited.
    let res = run(
        &["--max-traversers", "0", "-e", "g.v().to_list().len()"],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "6");
}

#[test]
fn memory_limit_fails_a_query_that_grows_past_it_and_rolls_back() {
    // The CLI installs the tracking allocator, so the budget compares the live heap: a loop that
    // doubles its value crosses a few megabytes quickly, a small query fits.
    let res = run(
        &[
            "--graph",
            "empty",
            "--memory-limit",
            "8000000",
            "-e",
            "g.v().count().next()",
            "-e",
            "g.addV(\"x\").inject(1).repeat(__.path()).times(40).count().next()",
        ],
        "",
    );
    assert_eq!(res.code, 1, "stdout: {}", res.stdout);
    assert_eq!(res.stdout.trim(), "0");
    assert!(
        res.stderr
            .contains("Resource limit 'memory.limit' exceeded (limit 8000000, reached "),
        "{}",
        res.stderr
    );
    assert!(res.stderr.contains("--memory-limit"), "{}", res.stderr);
    // The vertex the failed traversal added was rolled back.
    let res = run(
        &[
            "--graph",
            "empty",
            "--memory-limit",
            "8000000",
            "-e",
            "try { g.addV(\"x\").inject(1).repeat(__.path()).times(40).iterate() } catch {}",
            "-e",
            "g.v().count().next()",
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(
        res.stdout.trim().lines().last(),
        Some("0"),
        "{}",
        res.stdout
    );
    // "auto" without a cgroup limit, an explicit headroom, and 0 are no budget for a small query.
    for args in [
        &["--memory-limit", "auto", "--memory-headroom", "0"][..],
        &["--memory-limit", "0"][..],
    ] {
        let mut all = args.to_vec();
        all.extend(["-e", "g.v().count().next()"]);
        let res = run(&all, "");
        assert_eq!(res.code, 0, "stderr: {}", res.stderr);
        assert_eq!(res.stdout.trim(), "6");
    }
    let res = run(&["--memory-limit", "lots", "-e", "1"], "");
    assert_eq!(res.code, 2);
    assert!(
        res.stderr.contains("Invalid --memory-limit value"),
        "{}",
        res.stderr
    );
}

#[test]
fn spelling_flag_respells_profile_and_errors() {
    let res = run(
        &[
            "--spelling",
            "camel",
            "-e",
            "g.v().has_label(\"person\").out_e().count().profile()",
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert!(res.stdout.contains("outE()"), "{}", res.stdout);
    assert!(!res.stdout.contains("out_e()"), "{}", res.stdout);

    let res = run(
        &[
            "--spelling=camel",
            "-e",
            "g.v().has_label(\"person\").values(\"name\").sum().to_list()",
        ],
        "",
    );
    assert_eq!(res.code, 1);
    assert!(
        res.stderr
            .contains("at #2: v().hasLabel(\"person\").values(\"name\").sum()"),
        "{}",
        res.stderr
    );
    // the default is snake_case, and a query's own option wins over the flag
    let res = run(&["-e", "g.v().out_e().count().profile()"], "");
    assert!(res.stdout.contains("out_e()"), "{}", res.stdout);
    let res = run(
        &[
            "--spelling",
            "camel",
            "-e",
            "g.with(\"render.spelling\", \"snake\").v().out_e().count().profile()",
        ],
        "",
    );
    assert!(res.stdout.contains("out_e()"), "{}", res.stdout);
}

#[test]
fn invalid_spelling_is_a_usage_error() {
    let res = run(&["--spelling", "kebab", "-e", "1"], "");
    assert_eq!(res.code, 2);
    assert!(
        res.stderr.contains("Invalid spelling: kebab"),
        "{}",
        res.stderr
    );
}

#[test]
fn invalid_limit_value_is_a_usage_error() {
    let res = run(&["--max-operations", "many", "-e", "1"], "");
    assert_eq!(res.code, 2);
    assert!(
        res.stderr.contains("Invalid --max-operations value"),
        "{}",
        res.stderr
    );
}

#[test]
fn max_value_depth_raises_the_script_and_the_traversal_depth() {
    let deep = "let x = 1; for i in 0..200 { x = [x]; } g.inject(x).count().next()";
    let res = run(&["-e", deep], "");
    assert_eq!(res.code, 1);
    assert!(
        res.stderr
            .contains("Resource limit 'script.max_value_depth' exceeded (limit 128)"),
        "{}",
        res.stderr
    );
    assert!(
        res.stderr.contains("--max-value-depth 256"),
        "{}",
        res.stderr
    );
    let res = run(&["--max-value-depth", "300", "-e", deep], "");
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "1");
    // The traversal side follows: a loop that nests its value 200 levels deep.
    let nesting = "g.inject(1).repeat(__.fold()).times(200).count().next()";
    let res = run(&["-e", nesting], "");
    assert_eq!(res.code, 1);
    assert!(
        res.stderr.contains("traversal.max_value_depth"),
        "{}",
        res.stderr
    );
    let res = run(&["--max-value-depth", "300", "-e", nesting], "");
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
}

#[test]
fn deeply_nested_script_maps_do_not_overflow_the_stack() {
    // Rhai copies nested maps recursively; the session runs on a 64 MiB stack.
    let res = run(
        &["-e", "let x = 1; for i in 0..6000 { x = #{a: x}; } 7"],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "7");
}

#[test]
fn the_timeout_reaches_every_source_a_script_uses() {
    for source in ["__", "_g", "GraphSource::empty()"] {
        let res = run(
            &[
                "--timeout",
                "200",
                "-e",
                &format!("{source}.inject(1).repeat(__.group()).times(60).count().next()"),
            ],
            "",
        );
        assert_eq!(res.code, 1, "{source}: {}", res.stdout);
        assert!(
            res.stderr.contains("evaluationTimeout of 200 ms"),
            "{source}: {}",
            res.stderr
        );
    }
}

#[test]
fn the_step_string_limit_follows_the_script_string_limit() {
    let script =
        r#"let s = "x"; for i in 0..13 { s += s; } g.inject(s).replace("x", s).length().next()"#;
    // A trusted local tool: unlimited by default, like the script's own strings.
    let res = run(&["-e", script], "");
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "67108864");
    // --safe-limits and --max-string-size bound the strings steps build too.
    for flags in [vec!["--safe-limits"], vec!["--max-string-size", "1000000"]] {
        let mut args = flags.clone();
        args.extend(["-e", script]);
        let res = run(&args, "");
        assert_eq!(res.code, 1, "{flags:?}");
        assert!(
            res.stderr.contains("traversal.max_string_bytes"),
            "{flags:?}: {}",
            res.stderr
        );
    }
}

/// `graphersal store ...` and `--graph <dir>`: a store is created, scripts commit through its
/// write-ahead log, and every store command works on it.
#[test]
fn store_commands_and_scripts_on_a_store_directory() {
    let dir = temp_path("store");
    let _ = std::fs::remove_dir_all(&dir);
    let d = dir.to_str().unwrap();
    let res = run(&["--graph", d, "-e", "g.v().count().next()"], "");
    assert_eq!(res.code, 2, "not a store yet: {}", res.stderr);
    assert!(res.stderr.contains("--create-store"), "{}", res.stderr);

    let res = run(&["store", "create", d, "--from", "modern"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(res.stdout.contains("Created the store"), "{}", res.stdout);
    let res = run(
        &[
            "--graph",
            d,
            "-e",
            r#"g.add_v("person").property("name", "zoe").to_list(); g.mark("zoe")"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "{}", res.stderr);
    let res = run(&["--graph", d, "-e", "g.v().count().next()"], "");
    assert_eq!(res.stdout.trim(), "7", "{}", res.stderr);

    let res = run(&["store", "checkpoint", d, "--name", "seven"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    let res = run(&["store", "list", d], "");
    assert!(res.stdout.contains("seven"), "{}", res.stdout);
    let res = run(&["store", "marks", d], "");
    assert!(res.stdout.contains("zoe"), "{}", res.stdout);
    let res = run(&["store", "mark", d, "later"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    let res = run(&["store", "info", d], "");
    assert!(res.stdout.contains("commit      1"), "{}", res.stdout);
    assert!(res.stdout.contains("definitions 0"), "{}", res.stdout);
    // The catalog per kind: a compression rule and a saved query, defined through the DSL.
    let res = run(
        &[
            "--graph",
            d,
            "-e",
            r#"g.define_compression(#{name: "c", label: "person", path: "bio"})"#,
            "-e",
            r#"g.define_query(#{name: "q", body: "fn q() { 1 }"})"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "{}", res.stderr);
    let res = run(&["store", "info", d], "");
    assert!(
        res.stdout
            .contains("definitions 2 (compression 1, query 1)"),
        "{}",
        res.stdout
    );
    let res = run(
        &[
            "--graph",
            d,
            "-e",
            r#"g.drop_compression("c")"#,
            "-e",
            r#"g.drop_query("q")"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "{}", res.stderr);
    let res = run(&["store", "info", d], "");
    assert!(res.stdout.contains("closed cleanly"), "{}", res.stdout);
    let res = run(&["store", "verify", d], "");
    assert_eq!(res.code, 0, "{}{}", res.stdout, res.stderr);
    assert!(res.stdout.contains("ok: no damage found"));

    // Fork to the state before zoe, export, backup and restore through a ZIP.
    let fork = temp_path("store-fork");
    let _ = std::fs::remove_dir_all(&fork);
    let res = run(
        &[
            "store",
            "fork",
            d,
            fork.to_str().unwrap(),
            "--at-commit",
            "0",
        ],
        "",
    );
    assert_eq!(res.code, 0, "{}", res.stderr);
    let res = run(
        &[
            "--graph",
            fork.to_str().unwrap(),
            "-e",
            "g.v().count().next()",
        ],
        "",
    );
    assert_eq!(res.stdout.trim(), "6", "{}", res.stderr);
    assert!(!res.stdout.contains("Marks not carried"), "{}", res.stdout);
    // A fork at the mark: the fork carries no marks and says which ones it leaves behind.
    let fork_at_mark = temp_path("store-fork-mark");
    let _ = std::fs::remove_dir_all(&fork_at_mark);
    let res = run(
        &[
            "store",
            "fork",
            d,
            fork_at_mark.to_str().unwrap(),
            "--at-mark",
            "zoe",
        ],
        "",
    );
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(
        res.stdout.contains("Marks not carried (2;"),
        "{}",
        res.stdout
    );
    assert!(res.stdout.contains("  zoe @ commit 1"), "{}", res.stdout);
    assert!(res.stdout.contains("  later @ commit 1"), "{}", res.stdout);
    let _ = std::fs::remove_dir_all(&fork_at_mark);
    let gsnap = temp_path("store.gsnap");
    let res = run(&["store", "export", d, gsnap.to_str().unwrap()], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    let res = run(
        &[
            "--graph",
            gsnap.to_str().unwrap(),
            "-e",
            "g.v().count().next()",
        ],
        "",
    );
    assert_eq!(res.stdout.trim(), "7", "{}", res.stderr);
    let zip = temp_path("store.zip");
    let restored = temp_path("store-restored");
    let _ = std::fs::remove_file(&zip);
    let _ = std::fs::remove_dir_all(&restored);
    let res = run(&["store", "backup", d, zip.to_str().unwrap(), "--zip"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    let res = run(
        &[
            "store",
            "restore",
            zip.to_str().unwrap(),
            restored.to_str().unwrap(),
        ],
        "",
    );
    assert_eq!(res.code, 0, "{}", res.stderr);
    let res = run(
        &[
            "--graph",
            restored.to_str().unwrap(),
            "-e",
            "g.v().count().next()",
        ],
        "",
    );
    assert_eq!(res.stdout.trim(), "7", "{}", res.stderr);
    let res = run(&["store", "marks", restored.to_str().unwrap()], "");
    assert!(res.stdout.contains("later"), "{}", res.stdout);

    // Prune needs an explicit bound; usage errors exit 2.
    assert_eq!(run(&["store", "prune", d], "").code, 2);
    assert_eq!(run(&["store", "prune", d, "--up-to", "1"], "").code, 0);
    assert_eq!(run(&["store", "frobnicate", d], "").code, 2);

    // Damage is found by verify (exit 1).
    let wal = std::fs::read_dir(dir.join("wal"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut bytes = std::fs::read(&wal).unwrap();
    let last = bytes.len() - 3;
    bytes[last] ^= 0x40;
    std::fs::write(&wal, bytes).unwrap();
    let res = run(&["store", "verify", d], "");
    assert_eq!(res.code, 1, "{}", res.stdout);
    assert!(res.stdout.contains("DAMAGE"), "{}", res.stdout);

    for path in [&dir, &fork, &restored] {
        let _ = std::fs::remove_dir_all(path);
    }
    let _ = std::fs::remove_file(&zip);
    let _ = std::fs::remove_file(&gsnap);
}

/// `store prune` and `store compact` never make a mark unreachable without `--drop-marks`:
/// they name the marks, change nothing and exit 1; confirmed, they run and name them again.
#[test]
fn store_prune_and_compact_refuse_to_drop_marks_without_the_flag() {
    let dir = temp_path("store-drop-marks");
    let _ = std::fs::remove_dir_all(&dir);
    let d = dir.to_str().unwrap();
    assert_eq!(run(&["store", "create", d], "").code, 0);
    for round in 0..4 {
        let script = format!(r#"g.add_v("person").property("name", "p{round}").to_list()"#);
        let res = run(&["--graph", d, "-e", &script], "");
        assert_eq!(res.code, 0, "{}", res.stderr);
        if round < 2 {
            let res = run(&["store", "mark", d, &format!("m{round}")], "");
            assert_eq!(res.code, 0, "{}", res.stderr);
        }
        let res = run(&["store", "checkpoint", d], "");
        assert_eq!(res.code, 0, "{}", res.stderr);
    }
    let marks = || run(&["store", "marks", d], "").stdout;
    let res = run(&["store", "compact-advice", d], "");
    assert!(res.stdout.contains("marks lost  m0, m1"), "{}", res.stdout);

    for args in [
        &["store", "prune", d, "--up-to", "4"][..],
        &["store", "compact", d][..],
    ] {
        let res = run(args, "");
        assert_eq!(res.code, 1, "{args:?}: {}", res.stdout);
        assert!(
            res.stderr
                .contains("the mark(s) \"m0\", \"m1\" unreachable"),
            "{}",
            res.stderr
        );
        assert!(res.stderr.contains("--drop-marks"), "{}", res.stderr);
        assert!(res.stderr.contains("Nothing was changed"), "{}", res.stderr);
        assert!(marks().contains("m0"), "{}", marks());
    }
    // A prune that keeps the marks' history needs no flag.
    let res = run(&["store", "prune", d, "--up-to", "0"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    // Confirmed: the prune runs and names them.
    let res = run(&["store", "prune", d, "--up-to", "2", "--drop-marks"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(
        res.stdout
            .contains("No longer reachable (their history was pruned): the mark(s) m0."),
        "{}",
        res.stdout
    );
    let res = run(&["store", "compact", d, "--drop-marks"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(res.stdout.contains("the mark(s) m1."), "{}", res.stdout);
    assert!(!marks().contains("m1"), "{}", marks());
    // Nothing affected any more: no flag needed.
    let res = run(&["store", "compact", d], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `graphersal store backup` (full, then incremental), a backup opened read-only by `--graph`,
/// `store prune` and `info` on a backup, the refusals, and `store restore <BACKUP_DIR>`.
#[test]
fn store_backups_are_incremental_read_only_and_restorable() {
    let dir = temp_path("bk-store");
    let backup = temp_path("bk-backup");
    let other = temp_path("bk-other");
    for path in [&dir, &backup, &other] {
        let _ = std::fs::remove_dir_all(path);
    }
    let (d, b) = (dir.to_str().unwrap(), backup.to_str().unwrap());
    assert_eq!(run(&["store", "create", d, "--from", "modern"], "").code, 0);
    let res = run(&["store", "backup", d, b], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(res.stdout.contains("Full backup"), "{}", res.stdout);

    // Commits, a checkpoint and more commits: the increment copies only what is new.
    let write = |name: &str| {
        let script = format!(r#"g.add_v("person").property("name", "{name}").to_list()"#);
        let res = run(&["--graph", d, "-e", &script], "");
        assert_eq!(res.code, 0, "{}", res.stderr);
    };
    write("ann");
    let res = run(&["store", "backup", d, b], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(
        res.stdout.contains("Incremental backup") && res.stdout.contains("commits 1..1"),
        "{}",
        res.stdout
    );
    assert!(res.stdout.contains("bytes appended"), "{}", res.stdout);
    assert_eq!(run(&["store", "checkpoint", d], "").code, 0);
    write("bob");
    let res = run(&["store", "backup", d, b], "");
    assert!(
        res.stdout.contains("copied 1 snapshot(s) [1]"),
        "{}",
        res.stdout
    );
    let res = run(&["store", "backup", d, b], "");
    assert!(
        res.stdout.contains("nothing new since commit 2"),
        "{}",
        res.stdout
    );

    // The backup: read-only, every commit a target, info shows the marker.
    let res = run(&["store", "info", b], "");
    assert!(
        res.stdout.contains("backup      up to commit 2"),
        "{}",
        res.stdout
    );
    let res = run(&["--graph", b, "-e", "g.v().count().next()"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert_eq!(res.stdout.trim(), "8");
    assert!(res.stderr.contains("BACKUP"), "{}", res.stderr);
    let res = run(&["--graph", b, "-e", r#"g.add_v("x").to_list()"#], "");
    assert_eq!(res.code, 1);
    assert!(res.stderr.contains("is a backup"), "{}", res.stderr);
    let res = run(&["store", "mark", b, "nope"], "");
    assert_eq!(res.code, 1);
    assert!(res.stderr.contains("store restore"), "{}", res.stderr);
    let fork = temp_path("bk-fork");
    let _ = std::fs::remove_dir_all(&fork);
    let f = fork.to_str().unwrap();
    assert_eq!(
        run(&["store", "fork", b, f, "--at-commit", "1"], "").code,
        0
    );
    let res = run(&["--graph", f, "-e", "g.v().count().next()"], "");
    assert_eq!(res.stdout.trim(), "7", "{}", res.stderr);
    assert_eq!(run(&["store", "verify", b], "").code, 0);
    let res = run(&["store", "prune", b, "--up-to", "2"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);

    // Refusals: --full into a backup, another graph, usage.
    let res = run(&["store", "backup", d, b, "--full"], "");
    assert_eq!(res.code, 1);
    assert!(res.stderr.contains("not empty"), "{}", res.stderr);
    let o = other.to_str().unwrap();
    assert_eq!(run(&["store", "create", o], "").code, 0);
    let res = run(&["store", "backup", o, b], "");
    assert_eq!(res.code, 1);
    assert!(res.stderr.contains("another graph"), "{}", res.stderr);
    assert_eq!(
        run(&["store", "backup", d, b, "--zip", "--full"], "").code,
        2
    );

    // Restore in place: the live store, the same graph id, writable.
    let id_line = |path: &str| {
        run(&["store", "info", path], "")
            .stdout
            .lines()
            .find(|l| l.starts_with("graph id"))
            .unwrap()
            .to_string()
    };
    let res = run(&["store", "restore", b], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(res.stdout.contains("live store"), "{}", res.stdout);
    assert_eq!(id_line(b), id_line(d));
    let res = run(
        &[
            "--graph",
            b,
            "-e",
            r#"g.add_v("x").to_list(); g.v().count().next()"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert_eq!(res.stdout.trim().lines().last(), Some("9"));
    // A restored backup is a store: no longer a backup target.
    let res = run(&["store", "backup", d, b], "");
    assert_eq!(res.code, 1);
    assert!(res.stderr.contains("not a backup"), "{}", res.stderr);

    for path in [&dir, &backup, &other, &fork] {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// `graphersal store rollback | attic | repair` and maintenance mode on the command line.
#[test]
fn store_rollback_attic_and_repair_commands() {
    let dir = temp_path("store-rollback");
    let _ = std::fs::remove_dir_all(&dir);
    let d = dir.to_str().unwrap();
    assert_eq!(run(&["store", "create", d, "--from", "modern"], "").code, 0);
    let res = run(
        &[
            "--graph",
            d,
            "-e",
            r#"g.add_v("person").property("name", "zoe").to_list(); g.mark("zoe"); g.add_v("person").property("name", "ann").to_list()"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "{}", res.stderr);
    let count = |d: &str| run(&["--graph", d, "-e", "g.v().count().next()"], "").stdout;
    assert_eq!(count(d).trim(), "8");

    // A target is required; then the history after the mark moves to the attic.
    assert_eq!(run(&["store", "rollback", d], "").code, 2);
    let res = run(&["store", "rollback", d, "--at-mark", "zoe"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(res.stdout.contains("in the attic"), "{}", res.stdout);
    // It says how to undo it.
    assert!(
        res.stdout
            .contains(&format!("graphersal store attic {d} restore ")),
        "{}",
        res.stdout
    );
    assert_eq!(count(d).trim(), "7");
    let res = run(&["store", "attic", d], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    let id = res.stdout.split_whitespace().next().unwrap().to_string();
    assert!(res.stdout.contains("commits 2..=2"), "{}", res.stdout);
    let res = run(&["store", "attic", d, "changes", &id], "");
    assert!(res.stdout.contains("commit        2"), "{}", res.stdout);
    let res = run(&["store", "info", d], "");
    assert!(res.stdout.contains("attic       1"), "{}", res.stdout);
    let res = run(&["store", "attic", d, "restore", &id], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert_eq!(count(d).trim(), "8");
    // Delete mode: no attic entry.
    let res = run(
        &["store", "rollback", d, "--at-commit", "0", "--delete"],
        "",
    );
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(res.stdout.contains("cannot be undone"), "{}", res.stdout);
    assert_eq!(count(d).trim(), "6");
    assert!(run(&["store", "attic", d], "").stdout.contains("empty"));
    assert_eq!(run(&["store", "attic", d, "restore", "nope"], "").code, 1);
    assert_eq!(run(&["store", "attic", d, "frobnicate"], "").code, 2);

    // Damage: a flipped byte in the latest snapshot's vertex segment, with an older donor.
    // At the snapshot of commit 0 already: the checkpoint returns it.
    assert_eq!(run(&["store", "checkpoint", d], "").code, 0);
    let res = run(
        &[
            "--graph",
            d,
            "-e",
            r#"g.add_v("person").property("name", "eve").to_list()"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert_eq!(run(&["store", "checkpoint", d], "").code, 0);
    let latest = std::fs::read_dir(dir.join("snapshots"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .max()
        .unwrap();
    let segment = latest.join("v-000000.seg");
    let mut bytes = std::fs::read(&segment).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x10;
    std::fs::write(&segment, bytes).unwrap();

    let res = run(&["--graph", d, "-e", "g.v().count().next()"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert_eq!(
        res.stdout.trim(),
        "7",
        "the donor fills the chunk in: {}",
        res.stderr
    );
    assert!(res.stderr.contains("maintenance mode"), "{}", res.stderr);
    assert!(res.stderr.contains("store repair"), "{}", res.stderr);
    let res = run(&["--graph", d, "-e", r#"g.add_v("x").to_list()"#], "");
    assert_eq!(res.code, 1, "{}", res.stderr);
    assert!(res.stderr.contains("maintenance mode"), "{}", res.stderr);
    let res = run(&["store", "verify", d], "");
    assert_eq!(res.code, 1);
    assert!(res.stdout.contains("donor: snapshot"), "{}", res.stdout);

    let repaired = temp_path("store-repaired");
    let _ = std::fs::remove_dir_all(&repaired);
    let r = repaired.to_str().unwrap();
    assert_eq!(run(&["store", "repair", d], "").code, 2, "--to is required");
    let res = run(&["store", "repair", d, "--to", r], "");
    assert_eq!(res.code, 0, "{}{}", res.stdout, res.stderr);
    assert!(res.stdout.contains("nothing lost"), "{}", res.stdout);
    assert_eq!(count(r).trim(), "7");
    assert_eq!(run(&["store", "verify", r], "").code, 0);
    let res = run(&["--graph", r, "-e", r#"g.add_v("x").to_list()"#], "");
    assert_eq!(
        res.code, 0,
        "the repaired store is writable: {}",
        res.stderr
    );

    for path in [&dir, &repaired] {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// `graphersal store <COMMAND> --help` (and `-h`, `store help <COMMAND>`) prints that command's
/// usage on stdout with exit 0, for every command.
#[test]
fn every_store_command_has_its_own_help() {
    for command in [
        "create",
        "info",
        "list",
        "marks",
        "mark",
        "checkpoint",
        "verify",
        "prune",
        "fork",
        "backup",
        "restore",
        "export",
        "rollback",
        "attic",
        "repair",
    ] {
        for args in [
            vec!["store", command, "--help"],
            vec!["store", command, "-h"],
            vec!["store", command, "some/dir", "--help"],
            vec!["store", "help", command],
        ] {
            let res = run(&args, "");
            assert_eq!(res.code, 0, "{args:?}: {}", res.stderr);
            assert!(
                res.stdout
                    .starts_with(&format!("USAGE:\n    graphersal store {command}")),
                "{args:?}: {}",
                res.stdout
            );
        }
    }
    let res = run(&["store", "rollback", "--help"], "");
    assert!(res.stdout.contains("--at-time <TIME>"), "{}", res.stdout);
    assert!(res.stdout.contains("attic"), "{}", res.stdout);
    let res = run(&["store", "frobnicate", "--help"], "");
    assert_eq!(res.code, 2, "{}", res.stdout);
    assert!(
        res.stderr.contains("Unknown store command"),
        "{}",
        res.stderr
    );
    let res = run(&["store", "help"], "");
    assert!(
        res.stdout.contains("store <COMMAND> --help"),
        "{}",
        res.stdout
    );
}

/// `--at-time` takes an RFC 3339 date-time (also without seconds, with an offset) or
/// microseconds; listings print times in the same form, and a printed time targets its commit.
#[test]
fn store_targets_by_rfc_3339_time() {
    let dir = temp_path("store-at-time");
    let _ = std::fs::remove_dir_all(&dir);
    let d = dir.to_str().unwrap();
    assert_eq!(run(&["store", "create", d, "--from", "modern"], "").code, 0);
    let res = run(
        &[
            "--graph",
            d,
            "-e",
            r#"g.add_v("person").property("name", "zoe").to_list(); g.mark("zoe")"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "{}", res.stderr);
    // The mark's time, as `store marks` prints it: RFC 3339, whole seconds, UTC.
    let marks = run(&["store", "marks", d], "").stdout;
    let printed = marks.split_whitespace().nth(1).unwrap().to_string();
    assert!(
        printed.len() == 20 && printed.ends_with('Z') && printed.contains('T'),
        "{marks}"
    );
    let count = |d: &str| run(&["--graph", d, "-e", "g.v().count().next()"], "").stdout;
    let mut forks = Vec::new();
    let without_seconds = format!("{}Z", &printed[..16]);
    let with_offset = format!("{}+00:00", &printed[..19]);
    for (i, time) in [
        printed.as_str(),
        &without_seconds,
        &with_offset,
        "2000-01-01T00:00Z",
    ]
    .into_iter()
    .enumerate()
    {
        let fork = temp_path(&format!("store-at-time-fork{i}"));
        let _ = std::fs::remove_dir_all(&fork);
        let f = fork.to_str().unwrap().to_string();
        let res = run(&["store", "fork", d, &f, "--at-time", time], "");
        assert_eq!(res.code, 0, "{time}: {}", res.stderr);
        assert!(
            res.stdout.contains("to undo the fork, delete"),
            "{}",
            res.stdout
        );
        assert!(res.stdout.contains("--server"), "{}", res.stdout);
        // Before the first commit: the created state (commit 0).
        let expected = if i == 3 { "6" } else { "7" };
        assert_eq!(count(&f).trim(), expected, "{time}");
        forks.push(fork);
    }
    for bad in ["2025-10-09", "2025-10-09T08:53:20", "noon"] {
        let fork = temp_path("store-at-time-bad");
        let res = run(
            &["store", "fork", d, fork.to_str().unwrap(), "--at-time", bad],
            "",
        );
        assert_eq!(res.code, 2, "{bad}: {}", res.stdout);
        assert!(res.stderr.contains("RFC 3339"), "{}", res.stderr);
        assert!(!fork.exists());
    }
    // A bare date is refused with the explicit forms to write instead.
    let fork = temp_path("store-at-time-date");
    let res = run(
        &[
            "store",
            "fork",
            d,
            fork.to_str().unwrap(),
            "--at-time",
            "2025-10-09",
        ],
        "",
    );
    assert_eq!(res.code, 2, "{}", res.stdout);
    assert!(
        res.stderr.contains("a date without a time")
            && res
                .stderr
                .contains("2025-10-09T23:59:59Z for the end of that day in UTC")
            && res.stderr.contains("2025-10-09T00:00:00+02:00"),
        "{}",
        res.stderr
    );
    assert!(!fork.exists());
    for path in forks.iter().chain([&dir]) {
        let _ = std::fs::remove_dir_all(path);
    }
}

#[test]
fn store_commands_on_a_single_file_store() {
    let root = temp_path("single-file");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = |name: &str| root.join(name).to_str().unwrap().to_string();
    let file = path("graph.gstore");
    let f = file.as_str();
    // --create-store with a .gstore path makes a single file.
    let res = run(
        &["--graph", f, "--create-store", "-e", "g.v().count().next()"],
        "",
    );
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(std::path::Path::new(f).is_file());
    std::fs::remove_file(f).unwrap();

    let res = run(&["store", "create", f, "--from", "modern"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(
        res.stdout.contains("Created the single-file store"),
        "{}",
        res.stdout
    );
    let res = run(
        &[
            "--graph",
            f,
            "-e",
            r#"g.add_v("person").property("name", "zoe").to_list(); g.v().count().next()"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert_eq!(res.stdout.trim(), "7");
    for _ in 0..3 {
        let res = run(&["store", "checkpoint", f], "");
        assert_eq!(res.code, 0, "{}", res.stderr);
        let res = run(
            &[
                "--graph",
                f,
                "-e",
                r#"g.v().has_label("software").drop().to_list(); g.add_v("software").to_list()"#,
            ],
            "",
        );
        assert_eq!(res.code, 0, "{}", res.stderr);
    }
    let res = run(&["store", "info", f], "");
    assert!(res.stdout.contains("single file"), "{}", res.stdout);

    // The advice: nothing by the defaults, "compact now" with low thresholds.
    let res = run(&["store", "compact-advice", f], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(
        res.stdout.starts_with("no compaction needed"),
        "{}",
        res.stdout
    );
    assert!(
        res.stdout.contains("decided by  total_bytes"),
        "{}",
        res.stdout
    );
    let res = run(
        &[
            "store",
            "compact-advice",
            f,
            "--min-size",
            "0",
            "--min-reclaimable",
            "1",
            "--min-ratio",
            "0.01",
        ],
        "",
    );
    assert!(res.stdout.starts_with("compact now"), "{}", res.stdout);
    assert!(res.stdout.contains("prune "), "{}", res.stdout);
    let res = run(&["store", "compact-advice", f, "--min-ratio", "2"], "");
    assert_eq!(res.code, 2);
    let before = std::fs::metadata(f).unwrap().len();
    let res = run(&["store", "compact", f], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(res.stdout.contains("Compacted the file"), "{}", res.stdout);
    assert!(std::fs::metadata(f).unwrap().len() < before);
    let res = run(&["store", "verify", f], "");
    assert_eq!(res.code, 0, "{}{}", res.stdout, res.stderr);

    // Convert to a directory and back; backup into a single file and restore it.
    let dir = path("as-dir");
    let res = run(&["store", "convert", f, &dir], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(std::path::Path::new(&dir).is_dir());
    let back = path("back-plain");
    let res = run(&["store", "convert", &dir, &back, "--single-file"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(std::path::Path::new(&back).is_file());
    let res = run(&["--graph", &back, "-e", "g.v().count().next()"], "");
    assert_eq!(res.stdout.trim(), "6", "{}", res.stderr);
    let res = run(&["store", "convert", f, &dir], "");
    assert_eq!(res.code, 2, "an existing target is refused");
    let backup = path("backup.gstore");
    let res = run(&["store", "backup", f, &backup], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    let res = run(&["store", "restore", &backup], "");
    assert_eq!(res.code, 0, "{}{}", res.stdout, res.stderr);
    let res = run(&["--graph", &backup, "-e", "g.v().count().next()"], "");
    assert_eq!(res.stdout.trim(), "6", "{}", res.stderr);
    let res = run(&["store", "help", "compact-advice"], "");
    assert!(res.stdout.contains("--min-ratio"), "{}", res.stdout);
    let _ = std::fs::remove_dir_all(&root);
}

/// `graphersal store checkpoint <DIR>` merges on the CLOSED store (CP2 matrix row 9): the new
/// snapshot is merged from the last one and the WAL, a second run writes nothing, and while
/// another process holds the store it is refused as in use.
#[test]
fn store_checkpoint_merges_a_closed_store_and_is_refused_while_it_is_open() {
    let dir = temp_path("store-merge");
    let _ = std::fs::remove_dir_all(&dir);
    let d = dir.to_str().unwrap();
    assert_eq!(run(&["store", "create", d, "--from", "modern"], "").code, 0);
    let res = run(
        &[
            "--graph",
            d,
            "-e",
            r#"g.v("1").property("age", 30).to_list()"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "{}", res.stderr);
    let res = run(&["store", "checkpoint", d], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(
        res.stdout
            .contains("Snapshot at commit 1 (6 vertices, 6 edges)"),
        "{}",
        res.stdout
    );
    assert!(
        res.stdout
            .contains("Merged from the snapshot at commit 0 and 1 commits"),
        "{}",
        res.stdout
    );
    let res = run(&["store", "checkpoint", d], "");
    assert!(
        res.stdout.contains("no new snapshot written"),
        "{}",
        res.stdout
    );
    let res = run(
        &["--graph", d, "-e", r#"g.v("1").values("age").next()"#],
        "",
    );
    assert_eq!(res.stdout.trim(), "30", "{}", res.stderr);

    // Another process holds the store (a long script): refused, nothing written.
    let mut holder = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args([
            "--graph",
            d,
            "-e",
            "let x = 0; for i in 0..40000000 { x += 1; } x",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // (The holder opens the store first; the long script keeps it open for seconds.)
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let mut refused = None;
    for _ in 0..200 {
        let res = run(&["store", "checkpoint", d], "");
        if res.code != 0 {
            refused = Some(res);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // A script on the held store fails the same way: an operation failure (1), not a usage
    // error (2). (Run before the holder is stopped.)
    let script = run(&["--graph", d, "-e", "1"], "");
    let _ = holder.kill();
    let held = holder.wait_with_output().unwrap();
    let refused = refused.unwrap_or_else(|| {
        panic!(
            "never refused; holder: {:?} {}",
            held.status,
            String::from_utf8_lossy(&held.stderr)
        )
    });
    assert!(refused.stderr.contains("in use"), "{}", refused.stderr);
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert!(script.stderr.contains("in use"), "{}", script.stderr);
    assert_eq!(script.code, 1, "{}", script.stderr);
    assert_eq!(run(&["store", "verify", d], "").code, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `graphersal store` prints the library's store warnings (`tracing` events) on stderr, never
/// on stdout; `GRAPHERSAL_LOG` sets the level (`off` silences them).
#[test]
fn store_warnings_reach_stderr_and_not_stdout() {
    let dir = temp_path("store-log");
    let _ = std::fs::remove_dir_all(&dir);
    let d = dir.to_str().unwrap();
    assert_eq!(run(&["store", "create", d, "--from", "modern"], "").code, 0);
    let res = run(&["--graph", d, "-e", r#"g.add_v("x").to_list()"#], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    damage_newest_wal(&dir);
    // A writer open of a damaged store logs the maintenance mode as a warning.
    let with_log = |level: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_graphersal"));
        command.args(["store", "mark", d, "m1"]);
        match level {
            Some(level) => command.env("GRAPHERSAL_LOG", level),
            None => command.env_remove("GRAPHERSAL_LOG"),
        };
        let output = command.output().expect("graphersal terminates");
        (
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        )
    };
    let (stdout, stderr) = with_log(None);
    let line = "WARN graphersal::persist::store:";
    assert!(stderr.contains(line), "{stderr}");
    assert!(stderr.contains("maintenance mode"), "{stderr}");
    assert!(!stdout.contains("WARN"), "{stdout}");
    let (stdout, stderr) = with_log(Some("off"));
    assert!(!stderr.contains(line), "{stderr}");
    assert!(!stdout.contains("WARN"), "{stdout}");
    let (_, stderr) = with_log(Some("loud"));
    assert!(stderr.contains("ignoring GRAPHERSAL_LOG"), "{stderr}");
    assert!(stderr.contains(line), "{stderr}");
    // Every mode on a `--graph <store>` logs too (`-e`, `--in`); stdout stays the result.
    let res = run(&["--graph", d, "-e", "g.v().count().next()"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert_eq!(res.stdout.trim(), "6", "{}", res.stdout);
    assert!(res.stderr.contains(line), "{}", res.stderr);
    let res = run(&["--graph", d, "--in"], "g.v().count().next()");
    assert_eq!(res.stdout.trim(), "6", "{}", res.stdout);
    assert!(res.stderr.contains(line), "{}", res.stderr);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A read-write open that rewrites a missing or damaged `GRAPH` copy from the other one says
/// so in one `note:` line on stderr, for `--graph <store>` and the `store` commands alike.
#[test]
fn a_repaired_graph_file_copy_is_noted_on_stderr() {
    let dir = temp_path("store-graph-copy");
    let _ = std::fs::remove_dir_all(&dir);
    let d = dir.to_str().unwrap();
    assert_eq!(run(&["store", "create", d, "--from", "modern"], "").code, 0);
    std::fs::remove_file(dir.join("GRAPH.copy")).unwrap();
    let res = run(&["--graph", d, "-e", "g.v().count().next()"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert_eq!(res.stdout.trim(), "6");
    let notes: Vec<&str> = res
        .stderr
        .lines()
        .filter(|l| l.starts_with("note:") && l.contains("GRAPH.copy"))
        .collect();
    assert_eq!(notes.len(), 1, "{}", res.stderr);
    assert!(notes[0].contains("rewritten"), "{}", res.stderr);
    assert!(dir.join("GRAPH.copy").exists());
    // Intact again: no note.
    let res = run(&["--graph", d, "-e", "1"], "");
    assert!(!res.stderr.contains("GRAPH"), "{}", res.stderr);
    // A store command that opens read-write.
    std::fs::remove_file(dir.join("GRAPH")).unwrap();
    let res = run(&["store", "mark", d, "m1"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(
        res.stderr.contains("note:") && res.stderr.contains("GRAPH is not usable"),
        "{}",
        res.stderr
    );
    assert_eq!(run(&["store", "verify", d], "").code, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `store create` and `--create-store` into a non-empty directory (or an existing store) are
/// refused with their own message and help (exit 1), and nothing is written there.
#[test]
fn creating_a_store_in_a_non_empty_directory_is_refused() {
    let dir = temp_path("store-create-non-empty");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), "keep").unwrap();
    let d = dir.to_str().unwrap();
    for args in [
        vec!["store", "create", d],
        vec!["--graph", d, "--create-store", "-e", "1"],
    ] {
        let res = run(&args, "");
        assert_eq!(res.code, 1, "{args:?}: {}", res.stderr);
        assert!(
            res.stderr.contains(&format!(
                "cannot create a store in {d}: the directory is not empty"
            )),
            "{}",
            res.stderr
        );
        assert!(
            res.stderr.contains("choose an empty or new directory")
                && res.stderr.contains(&format!("graphersal store info {d}")),
            "{}",
            res.stderr
        );
        assert!(
            !res.stderr.contains("not a Graphersal store"),
            "{}",
            res.stderr
        );
    }
    let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
    assert_eq!(entries.len(), 1);
    // An existing store: "a store already".
    std::fs::remove_file(dir.join("notes.txt")).unwrap();
    assert_eq!(run(&["store", "create", d], "").code, 0);
    let res = run(&["store", "create", d], "");
    assert_eq!(res.code, 1, "{}", res.stderr);
    assert!(
        res.stderr.contains("it is a store already"),
        "{}",
        res.stderr
    );
    // `--create-store` on an existing store just opens it.
    let res = run(&["--graph", d, "--create-store", "-e", "1"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Flips a byte of the last record of the store's newest WAL segment (a failing disk).
fn damage_newest_wal(dir: &std::path::Path) {
    let wal = std::fs::read_dir(dir.join("wal"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .max()
        .unwrap();
    let mut bytes = std::fs::read(&wal).unwrap();
    let at = bytes.len() - 5;
    bytes[at] ^= 0x40;
    std::fs::write(&wal, bytes).unwrap();
}

/// The damage policy is a creation parameter (`store create --on-damage`, `--create-store
/// --on-damage`), `store info` lists the creation parameters, an existing store refuses another
/// policy; a backup of a damaged store fails with the report and recommends repairing the
/// store; `--from-memory` explains where it works; a ZIP backup is read back.
#[test]
fn store_damage_policy_and_verified_backups_on_the_command_line() {
    let dir = temp_path("bk1-store");
    let created = temp_path("bk1-created");
    let backup = temp_path("bk1-backup");
    let zip = temp_path("bk1-backup.zip");
    for path in [&dir, &created, &backup] {
        let _ = std::fs::remove_dir_all(path);
    }
    let _ = std::fs::remove_file(&zip);
    let (d, c, b, z) = (
        dir.to_str().unwrap(),
        created.to_str().unwrap(),
        backup.to_str().unwrap(),
        zip.to_str().unwrap(),
    );
    let res = run(
        &[
            "store",
            "create",
            d,
            "--from",
            "modern",
            "--on-damage",
            "continue",
        ],
        "",
    );
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(
        res.stdout.contains("damage policy continue"),
        "{}",
        res.stdout
    );
    let res = run(&["store", "info", d], "");
    assert!(res.stdout.contains("fixed at creation"), "{}", res.stdout);
    assert!(res.stdout.contains("  store id      "), "{}", res.stdout);
    assert!(
        res.stdout.contains("on damage     continue"),
        "{}",
        res.stdout
    );
    assert!(
        res.stdout.contains("chunk size    1048576 bytes"),
        "{}",
        res.stdout
    );
    let res = run(&["store", "create", c, "--on-damage", "sometimes"], "");
    assert_eq!(res.code, 2, "{}", res.stderr);
    // --create-store --on-damage; an existing store refuses another policy.
    let res = run(
        &[
            "--graph",
            c,
            "--create-store",
            "--on-damage",
            "maintenance",
            "-e",
            "1",
        ],
        "",
    );
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(
        res.stderr.contains("damage policy maintenance"),
        "{}",
        res.stderr
    );
    let res = run(
        &[
            "--graph",
            d,
            "--create-store",
            "--on-damage",
            "maintenance",
            "-e",
            "1",
        ],
        "",
    );
    assert_eq!(res.code, 2, "{}", res.stderr);
    assert!(
        res.stderr.contains("fixed when a store is created"),
        "{}",
        res.stderr
    );
    let res = run(&["--graph", d, "--on-damage", "maintenance", "-e", "1"], "");
    assert_eq!(res.code, 2, "{}", res.stderr);
    // --from-memory needs the process that has the store open.
    let res = run(&["store", "backup", d, b, "--from-memory"], "");
    assert_eq!(res.code, 2);
    assert!(
        res.stderr.contains("OPEN IN THIS PROCESS"),
        "{}",
        res.stderr
    );
    // A ZIP backup is checked when read back.
    let res = run(&["store", "backup", d, z, "--zip"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(
        res.stdout.contains("every checksum read back"),
        "{}",
        res.stdout
    );
    // Damage: refused with the report, nothing kept, the store is to be repaired.
    let res = run(
        &[
            "--graph",
            d,
            "-e",
            r#"g.add_v("person").property("name", "ann").to_list()"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "{}", res.stderr);
    damage_newest_wal(&dir);
    let res = run(&["store", "backup", d, b], "");
    assert_eq!(res.code, 1, "{}", res.stdout);
    assert!(res.stderr.contains("is damaged"), "{}", res.stderr);
    assert!(res.stderr.contains("store repair"), "{}", res.stderr);
    assert!(!backup.exists());
    let res = run(&["store", "backup", d, &format!("{z}.2"), "--zip"], "");
    assert_eq!(res.code, 1, "{}", res.stdout);
    assert!(!std::path::Path::new(&format!("{z}.2")).exists());
    for path in [&dir, &created] {
        let _ = std::fs::remove_dir_all(path);
    }
    let _ = std::fs::remove_file(&zip);
}

#[test]
fn store_verify_and_repair_say_the_lineage_is_approximate_when_both_graph_copies_are_damaged() {
    let dir = temp_path("store-both-graph-copies");
    let _ = std::fs::remove_dir_all(&dir);
    let d = dir.to_str().unwrap();
    assert_eq!(run(&["store", "create", d, "--from", "modern"], "").code, 0);
    for name in ["GRAPH", "GRAPH.copy"] {
        let path = dir.join(name);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[20] ^= 0xff;
        std::fs::write(&path, bytes).unwrap();
    }
    let note = "lineage before commit 0 is approximate (both GRAPH copies damaged)";
    let res = run(&["store", "verify", d], "");
    assert_eq!(res.code, 1, "{}", res.stderr);
    assert!(res.stdout.contains(note), "{}", res.stdout);
    assert!(res.stderr.contains("store repair"), "{}", res.stderr);
    let repaired = temp_path("store-both-graph-copies-repaired");
    let _ = std::fs::remove_dir_all(&repaired);
    let res = run(
        &["store", "repair", d, "--to", repaired.to_str().unwrap()],
        "",
    );
    assert_eq!(res.code, 0, "{}{}", res.stdout, res.stderr);
    assert!(res.stdout.contains(note), "{}", res.stdout);
    for path in [&dir, &repaired] {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// A schema file for the `--schema` tests: `thing.uid` is a `uuid`, `thing.n` an `int64`.
const THING_SCHEMA: &str = r#"{"mode": "open", "vertices": {"thing": {"schema": {"type": "object",
    "properties": {"uid": {"type": "string", "format": "uuid"}, "n": {"type": "integer"}},
    "required": ["uid"]}}}}"#;

/// Writes `THING_SCHEMA` and a GraphML file with one `thing` (its uid is text in GraphML).
fn schema_and_graphml(name: &str) -> (PathBuf, PathBuf) {
    let schema = temp_path(&format!("{name}_schema.json"));
    std::fs::write(&schema, THING_SCHEMA).unwrap();
    let graphml = temp_path(&format!("{name}.graphml"));
    let res = run(
        &[
            "--graph",
            "empty",
            "-e",
            &format!(
                r#"g.add_v("thing").property("uid", "6f1c2d3e-4a5b-4c6d-8e7f-901234567890").property("n", 3).next(); g.export_graphml({:?})"#,
                graphml.to_str().unwrap()
            ),
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    (schema, graphml)
}

#[test]
fn schema_flag_types_the_imported_graph() {
    let (schema, graphml) = schema_and_graphml("schema_types");
    let uid_type = r#"type_of(g.v().values("uid").next())"#;
    // Without the schema the uid stays a string (GraphML carries no schema).
    let res = run(&["--graph", graphml.to_str().unwrap(), "-e", uid_type], "");
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "string");
    // With it the schema is set first and the import restores the declared uuid.
    let res = run(
        &[
            "--graph",
            graphml.to_str().unwrap(),
            "--schema",
            schema.to_str().unwrap(),
            "-e",
            uid_type,
            "-e",
            r#"g.get_schema()["mode"]"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(
        res.stdout.trim().lines().collect::<Vec<_>>(),
        ["Uuid", "open"]
    );
    // --schema-mode overrides the file's mode, and the mode is enforced afterwards.
    let res = run(
        &[
            "--graph",
            graphml.to_str().unwrap(),
            "--schema",
            schema.to_str().unwrap(),
            "--schema-mode",
            "closed",
            "-e",
            r#"g.get_schema()["mode"]"#,
            "-e",
            r#"g.add_v("robot").next()"#,
        ],
        "",
    );
    assert_eq!(res.code, 1, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "closed");
    assert!(res.stderr.contains("robot"), "stderr: {}", res.stderr);
    // A GraphSON file works the same way.
    let graphson = temp_path("schema_types.json");
    let res = run(
        &[
            "--graph",
            graphml.to_str().unwrap(),
            "-e",
            &format!("g.export_graphson({:?})", graphson.to_str().unwrap()),
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    let res = run(
        &[
            "--graph",
            graphson.to_str().unwrap(),
            "--schema",
            schema.to_str().unwrap(),
            "-e",
            r#"g.v().values("n").next()"#,
            "-e",
            r#"g.get_schema()["mode"]"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim().lines().collect::<Vec<_>>(), ["3", "open"]);
    for path in [schema, graphml, graphson] {
        let _ = std::fs::remove_file(path);
    }
}

#[test]
fn schema_flag_applies_to_a_sample_graph_and_reports_violations() {
    let (schema, graphml) = schema_and_graphml("schema_violation");
    // A sample graph gets the schema (open: the undeclared labels stay allowed).
    let res = run(
        &[
            "--graph",
            "modern",
            "--schema",
            schema.to_str().unwrap(),
            "-e",
            r#"g.get_schema()["mode"]"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "open");
    // Closed: Modern's labels are not declared, the start fails with the violation report.
    let res = run(
        &[
            "--graph",
            "modern",
            "--schema",
            schema.to_str().unwrap(),
            "--schema-mode",
            "closed",
            "-e",
            "1",
        ],
        "",
    );
    assert_eq!(res.code, 1, "stderr: {}", res.stderr);
    assert!(res.stdout.is_empty(), "stdout: {}", res.stdout);
    assert!(
        res.stderr.contains("vertex label person is not declared"),
        "stderr: {}",
        res.stderr
    );
    assert!(
        res.stderr.contains("does not fit the schema file"),
        "stderr: {}",
        res.stderr
    );
    // A file whose data violates the schema: exit 1 as well.
    let closed = temp_path("schema_violation_closed.json");
    std::fs::write(
        &closed,
        r#"{"mode": "closed", "vertices": {"other": {"schema": {"type": "object"}}}}"#,
    )
    .unwrap();
    let res = run(
        &[
            "--graph",
            graphml.to_str().unwrap(),
            "--schema",
            closed.to_str().unwrap(),
            "-e",
            "1",
        ],
        "",
    );
    assert_eq!(res.code, 1, "stderr: {}", res.stderr);
    assert!(res.stderr.contains("thing"), "stderr: {}", res.stderr);
    for path in [schema, graphml, closed] {
        let _ = std::fs::remove_file(path);
    }
}

#[test]
fn schema_flag_refuses_invalid_files_and_combinations() {
    let schema_path = temp_path("schema_refused.json");
    std::fs::write(&schema_path, THING_SCHEMA).unwrap();
    let schema = schema_path.to_str().unwrap();
    let usage = |args: &[&str], needle: &str| {
        let res = run(args, "");
        assert_eq!(res.code, 2, "{args:?}: stderr: {}", res.stderr);
        assert!(res.stdout.is_empty(), "{args:?}: stdout: {}", res.stdout);
        assert!(
            res.stderr.contains(needle),
            "{args:?}: stderr: {}",
            res.stderr
        );
    };
    let missing = temp_path("schema_refused_missing.json");
    usage(
        &["--schema", missing.to_str().unwrap(), "-e", "1"],
        "cannot read the schema file",
    );
    let invalid = temp_path("schema_refused_invalid.json");
    std::fs::write(&invalid, r#"{"vertices": 3}"#).unwrap();
    usage(
        &["--schema", invalid.to_str().unwrap(), "-e", "1"],
        "Invalid schema at /vertices",
    );
    usage(
        &["--schema-mode", "open", "-e", "1"],
        "--schema-mode needs --schema",
    );
    usage(
        &["--schema", schema, "--schema-mode", "strict", "-e", "1"],
        "Invalid --schema-mode value: strict",
    );
    // A Store and a packed snapshot carry their own schema.
    let store = temp_path("schema_refused_store");
    usage(
        &[
            "--graph",
            store.to_str().unwrap(),
            "--schema",
            schema,
            "-e",
            "1",
        ],
        "a Store carries its own schema",
    );
    usage(
        &[
            "--graph",
            store.to_str().unwrap(),
            "--create-store",
            "--schema",
            schema,
            "-e",
            "1",
        ],
        "a Store carries its own schema",
    );
    assert!(!store.exists());
    let snapshot = temp_path("schema_refused.gsnap");
    usage(
        &[
            "--graph",
            snapshot.to_str().unwrap(),
            "--schema",
            schema,
            "-e",
            "1",
        ],
        "a packed snapshot carries its own schema",
    );
    // The help text documents both flags.
    let help = run(&["--help"], "");
    assert!(help.stdout.contains("--schema <FILE>"), "{}", help.stdout);
    assert!(help.stdout.contains("--schema-mode <M>"), "{}", help.stdout);
    for path in [schema_path.clone(), invalid] {
        let _ = std::fs::remove_file(path);
    }
}

/// A graph value as the script's final value is shown as its vertices and edges, like
/// `cap("sg")` shows a graph inside a traversal; `next()` keeps returning the graph itself.
#[test]
fn a_graph_value_as_the_final_value_lists_its_elements() {
    let res = run(&["-e", "g.e(0).to_graph()"], "");
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert!(res.stdout.contains("edges"), "stdout: {}", res.stdout);
    assert!(
        res.stdout.contains("[v[1], v[2]]"),
        "stdout: {}",
        res.stdout
    );
    assert!(
        res.stdout.contains("e[0][1-knows->2]"),
        "stdout: {}",
        res.stdout
    );

    let res = run(
        &[
            "--format",
            "json",
            "-e",
            r#"g.e().subgraph("sg").cap("sg").next()"#,
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert!(
        res.stdout.contains(r#""vertices": ["#),
        "stdout: {}",
        res.stdout
    );
    assert!(
        res.stdout.contains(r#""name": "peter""#),
        "stdout: {}",
        res.stdout
    );
    assert!(
        res.stdout.contains(r#""outV": "6""#),
        "stdout: {}",
        res.stdout
    );

    // The data terminal is unchanged: `next()` is still a graph a script traverses.
    let res = run(
        &[
            "-e",
            r#"let sub = g.e(0).subgraph("sg").cap("sg").next(); sub.v().count().next()"#,
        ],
        "",
    );
    assert_eq!(res.stdout.trim(), "2", "stderr: {}", res.stderr);
}

/// The display limits apply to a graph value: each element list is cut to `--max-items`, and the
/// notice goes to stderr.
#[test]
fn a_graph_value_is_bounded_by_the_display_limits() {
    let res = run(&["--max-items", "2", "-e", "g"], "");
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert!(
        res.stdout.contains("[v[1], v[2], …]"),
        "stdout: {}",
        res.stdout
    );
    assert!(!res.stdout.contains("v[3]"), "stdout: {}", res.stdout);
    assert!(
        res.stderr.contains("nested collections were cut to 2"),
        "stderr: {}",
        res.stderr
    );
}

/// An error location shows the user's query: the `limit(n) [display]` a bounded visualizer
/// appends is not part of it.
#[test]
fn an_error_location_leaves_out_the_display_limit() {
    let res = run(&["-e", r#"g.inject(1).math("value + 1")"#], "");
    assert_eq!(res.code, 1, "stdout: {}", res.stdout);
    assert!(
        res.stderr
            .contains("  at #1: inject(1).math(\"value + 1\")\n"),
        "stderr: {}",
        res.stderr
    );
    assert!(!res.stderr.contains("[display]"), "stderr: {}", res.stderr);
    assert!(!res.stderr.contains("limit("), "stderr: {}", res.stderr);
}

/// A pipe whose reading end is already closed: every write to it fails with `EPIPE`.
fn closed_pipe() -> std::io::PipeWriter {
    let (reader, writer) = std::io::pipe().expect("a pipe");
    drop(reader);
    writer
}

/// `graphersal -e '..' | head -1`: the reader goes away after the first line of a long result.
/// The run stops quietly (no panic, the next script is not run) with exit code 141.
#[test]
fn a_closed_stdout_ends_a_one_shot_run_quietly_with_141() {
    use std::io::BufRead;
    let mut child = Command::new(env!("CARGO_BIN_EXE_graphersal"))
        .args([
            "-e",
            r#"let s = ""; for i in 0..200000 { s += "line\n"; } s"#,
            "-e",
            r#"g.add_v("never").to_list()"#,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("graphersal binary starts");
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut first = String::new();
    std::io::BufReader::new(stdout)
        .read_line(&mut first)
        .expect("the first line");
    // The reader is dropped here: the rest of the result meets a closed pipe.
    let output = child.wait_with_output().expect("graphersal terminates");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(first.contains("line"), "{first:?}");
    assert_eq!(output.status.code(), Some(141), "stderr: {stderr}");
    assert!(!stderr.contains("panicked"), "stderr: {stderr}");
    assert!(stderr.is_empty(), "nothing to report: {stderr}");
}

/// Help, version and a short result on a closed stdout (and a closed stderr): exit code 141,
/// never a panic (101); a failing script keeps its own code.
#[test]
fn closed_stdout_and_stderr_never_panic() {
    for (args, expected) in [
        (&["--help"][..], 141),
        (&["--version"][..], 141),
        (&["-e", "g.v().count().next()"][..], 141),
        // The diagnostic is lost with stderr; the code stays the script's.
        (&["-e", "g.v().fail_here()"][..], 1),
    ] {
        let status = Command::new(env!("CARGO_BIN_EXE_graphersal"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(closed_pipe())
            .stderr(closed_pipe())
            .status()
            .expect("graphersal terminates");
        assert_eq!(status.code(), Some(expected), "{args:?}");
    }
}

/// `graphersal store ...` with nobody reading stdout or stderr: the command completes, closes
/// the store cleanly and exits with 141.
#[test]
fn store_commands_survive_closed_pipes() {
    let dir = temp_path("store_closed_pipe");
    let _ = std::fs::remove_dir_all(&dir);
    let d = dir.to_str().unwrap();
    for args in [
        &["store", "create", d, "--from", "modern"][..],
        &["store", "info", d][..],
        &["store", "help"][..],
    ] {
        let status = Command::new(env!("CARGO_BIN_EXE_graphersal"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(closed_pipe())
            .stderr(closed_pipe())
            .status()
            .expect("graphersal terminates");
        assert_eq!(status.code(), Some(141), "{args:?}");
    }
    // The store was created and closed cleanly despite the lost output.
    let res = run(&["store", "verify", d], "");
    assert_eq!(res.code, 0, "{} {}", res.stdout, res.stderr);
    let res = run(&["--graph", d, "-e", "g.v().count().next()"], "");
    assert_eq!(res.stdout.trim(), "6", "{}", res.stderr);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Copies a directory tree (a store fixture) to `to`.
fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

/// `graphersal store upgrade`: a store written by the released 0.1.0 (the golden fixture of
/// `graphersal`'s tests) is refused by `--graph` with the command that upgrades it; the
/// upgrade into a copy and in place both work without the older binary.
#[test]
fn store_upgrade_converts_a_store_of_graphersal_0_1() {
    let fixtures =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../graphersal/tests/data/v0.1.0");
    let root = temp_path("store-upgrade");
    let _ = std::fs::remove_dir_all(&root);
    let old = root.join("old");
    copy_tree(&fixtures.join("store"), &old);
    let old = old.to_string_lossy().to_string();
    let new = root.join("new").to_string_lossy().to_string();

    let res = run(&["--graph", &old, "-e", "g.v().count().next()"], "");
    assert_eq!(res.code, 1);
    assert!(
        res.stderr
            .contains(&format!("graphersal store upgrade {old} <new_dir>")),
        "{}",
        res.stderr
    );
    // The dev server does not start on it either, with the same help.
    let res = run(&["--graph", &old, "--server", "--port", "0"], "");
    assert_eq!(res.code, 1);
    assert!(
        res.stderr.contains("graphersal store upgrade"),
        "{}",
        res.stderr
    );
    let res = run(&["store", "upgrade", &old], "");
    assert_eq!(
        res.code, 2,
        "neither a target nor --in-place: {}",
        res.stderr
    );
    let res = run(&["store", "upgrade", &old, &new], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(
        res.stdout.contains("store format version 2"),
        "{}",
        res.stdout
    );
    assert!(
        res.stdout
            .contains(&format!("Use it: graphersal --graph {new}"))
    );
    let res = run(&["--graph", &new, "-e", "g.v().count().next()"], "");
    assert_eq!(res.stdout.trim(), "9", "{}", res.stderr);
    let res = run(&["store", "marks", &new], "");
    assert!(res.stdout.contains("before-rollback"), "{}", res.stdout);
    // An existing target is refused; the older store is unchanged and still older.
    let res = run(&["store", "upgrade", &old, &new], "");
    assert_eq!(res.code, 2, "{}", res.stderr);
    let res = run(&["store", "info", &old], "");
    assert_eq!(res.code, 1, "{}", res.stdout);

    // In place, then again (nothing to do).
    let res = run(&["store", "upgrade", &old, "--in-place"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(res.stdout.contains("in place"), "{}", res.stdout);
    let res = run(&["store", "upgrade", &old, "--in-place"], "");
    assert_eq!(res.code, 0, "{}", res.stderr);
    assert!(res.stdout.contains("nothing to do"), "{}", res.stdout);
    let res = run(&["store", "verify", &old], "");
    assert_eq!(res.code, 0, "{}{}", res.stdout, res.stderr);
    let res = run(&["store", "help", "upgrade"], "");
    assert!(res.stdout.contains("--in-place"), "{}", res.stdout);
    let _ = std::fs::remove_dir_all(&root);
}

/// CRC-32C (Castagnoli), bitwise: the newer-version test recomputes header checksums.
fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x82F6_3B78
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Files of a newer Graphersal (made by patching the version words of files this build wrote,
/// checksums recomputed: a store of store format version 4, a single-file store of container
/// version 2, a packed snapshot of the next file format version) are refused as "written by a newer
/// Graphersal" by the `store` commands and by `--graph`: exit 1, never DAMAGE, never
/// "Corrupt", no repair offered, nothing written.
#[test]
fn store_commands_refuse_files_of_a_newer_graphersal() {
    let dir = temp_path("newer-store");
    let file = temp_path("newer.gstore");
    let snap = temp_path("newer.gsnap");
    let target = temp_path("newer-target");
    for path in [&dir, &target] {
        let _ = std::fs::remove_dir_all(path);
    }
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(&snap);
    let (d, f, s, t) = (
        dir.to_str().unwrap(),
        file.to_str().unwrap(),
        snap.to_str().unwrap(),
        target.to_str().unwrap(),
    );
    for args in [
        vec!["store", "create", d, "--from", "modern"],
        vec!["store", "create", f, "--from", "modern"],
        vec!["store", "export", d, s],
    ] {
        let res = run(&args, "");
        assert_eq!(res.code, 0, "{args:?}: {}", res.stderr);
    }
    // GRAPH: store format version at 8, CRC of the 256-byte fixed part at 252.
    for name in ["GRAPH", "GRAPH.copy"] {
        let path = dir.join(name);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[8..12].copy_from_slice(&4u32.to_le_bytes());
        let crc = crc32c(&bytes[..252]);
        bytes[252..256].copy_from_slice(&crc.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
    }
    let _ = std::fs::remove_file(dir.join("LOCK"));
    // The single file's preamble: container version at 8 (no checksum).
    let mut bytes = std::fs::read(&file).unwrap();
    bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
    std::fs::write(&file, bytes).unwrap();
    // The packed snapshot's header: file format version at 8.
    let mut bytes = std::fs::read(&snap).unwrap();
    bytes[8..12].copy_from_slice(&(graphersal::persist::FORMAT_VERSION + 1).to_le_bytes());
    std::fs::write(&snap, bytes).unwrap();

    let store_bytes = std::fs::read(dir.join("GRAPH")).unwrap();
    let file_bytes = std::fs::read(&file).unwrap();
    let count = "g.v().count().next()";
    let runs: Vec<(&str, Vec<&str>)> = vec![
        ("info", vec!["store", "info", d]),
        ("verify", vec!["store", "verify", d]),
        ("list", vec!["store", "list", d]),
        ("marks", vec!["store", "marks", d]),
        ("repair", vec!["store", "repair", d, "--to", t]),
        ("backup", vec!["store", "backup", d, t]),
        ("checkpoint", vec!["store", "checkpoint", d]),
        ("convert", vec!["store", "convert", d, t]),
        ("upgrade", vec!["store", "upgrade", d, t]),
        (
            "upgrade in place",
            vec!["store", "upgrade", d, "--in-place"],
        ),
        ("--graph", vec!["--graph", d, "-e", count]),
        ("info single file", vec!["store", "info", f]),
        ("verify single file", vec!["store", "verify", f]),
        (
            "upgrade single file",
            vec!["store", "upgrade", f, "--in-place"],
        ),
        ("--graph single file", vec!["--graph", f, "-e", count]),
        ("--graph gsnap", vec!["--graph", s, "-e", count]),
        (
            "create --from gsnap",
            vec!["store", "create", t, "--from", s],
        ),
    ];
    for (what, args) in runs {
        let res = run(&args, "");
        let all = format!("{}{}", res.stdout, res.stderr);
        // A `--graph` FILE that cannot be loaded is an invalid invocation (2), as for any
        // unreadable graph file; every store operation fails with 1.
        let expected = if what == "--graph gsnap" { 2 } else { 1 };
        assert_eq!(res.code, expected, "{what}: {all}");
        assert!(
            all.contains("written by a newer Graphersal"),
            "{what}: {all}"
        );
        assert!(!all.contains("DAMAGE"), "{what}: {all}");
        assert!(!all.contains("Corrupt"), "{what}: {all}");
        assert!(!all.contains("store repair"), "{what}: {all}");
        assert!(!target.exists(), "{what}: nothing created at the target");
    }
    assert_eq!(std::fs::read(dir.join("GRAPH")).unwrap(), store_bytes);
    assert!(!dir.join("LOCK").exists(), "no LOCK file created");
    assert_eq!(std::fs::read(&file).unwrap(), file_bytes);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(&snap);
}

/// `--embedder`, `--default-embedder` and `graphersal embed-model download|list` on a TINY
/// synthetic Model2Vec model written by the test; the download runs against a local mock of the
/// Hugging Face endpoint (`GRAPHERSAL_HF_ENDPOINT`), never the network.
#[cfg(feature = "model2vec")]
mod embed_models {
    use super::{Run, temp_path};
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};

    const VOCAB: [&str; 6] = ["[UNK]", "graph", "vector", "marko", "lop", "ripple"];

    fn config_json() -> Vec<u8> {
        br#"{"model_type": "model2vec", "normalize": true}"#.to_vec()
    }

    fn tokenizer_json() -> Vec<u8> {
        let entries: Vec<String> = VOCAB
            .iter()
            .enumerate()
            .map(|(id, token)| format!("{token:?}: {id}"))
            .collect();
        format!(
            r#"{{"version": "1.0", "truncation": null, "padding": null, "added_tokens": [],
                "normalizer": {{"type": "Lowercase"}}, "pre_tokenizer": {{"type": "Whitespace"}},
                "post_processor": null, "decoder": null,
                "model": {{"type": "WordLevel", "vocab": {{{}}}, "unk_token": "[UNK]"}}}}"#,
            entries.join(", ")
        )
        .into_bytes()
    }

    /// A safetensors file with one F32 `embeddings` tensor of 6 x 3.
    fn model_safetensors() -> Vec<u8> {
        let data: Vec<u8> = (0..18)
            .map(|i| ((i % 7) as f32 + 0.5) * if i % 2 == 0 { 1.0 } else { -1.0 })
            .flat_map(f32::to_le_bytes)
            .collect();
        let mut header = format!(
            r#"{{"embeddings": {{"dtype": "F32", "shape": [6, 3], "data_offsets": [0, {}]}}}}"#,
            data.len()
        );
        while header.len() % 8 != 0 {
            header.push(' ');
        }
        let mut out = (header.len() as u64).to_le_bytes().to_vec();
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(&data);
        out
    }

    fn fresh_dir(name: &str) -> PathBuf {
        let dir = temp_path(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_model(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("config.json"), config_json()).unwrap();
        std::fs::write(dir.join("tokenizer.json"), tokenizer_json()).unwrap();
        std::fs::write(dir.join("model.safetensors"), model_safetensors()).unwrap();
    }

    /// Runs the binary with `envs` set (and every variable the resolution reads cleared).
    fn run_env(args: &[&str], envs: &[(&str, &str)]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_graphersal"));
        command
            .args(args)
            .env_remove("GRAPHERSAL_EMBED_MODELS")
            .env_remove("GRAPHERSAL_HF_ENDPOINT")
            .env_remove("HF_TOKEN")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in envs {
            command.env(key, value);
        }
        let output = command.output().expect("graphersal runs");
        Run {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            code: output.status.code().unwrap_or(-1),
        }
    }

    #[test]
    fn embedder_from_a_directory_becomes_the_default() {
        let dir = fresh_dir("m2v_path");
        write_model(&dir);
        let spec = format!("tiny={}", dir.display());
        let res = run_env(
            &[
                "--embedder",
                &spec,
                "--default-embedder",
                "tiny",
                "-e",
                r#"g.embedders().map(|e| e.name + " " + e.model + " " + e.dimension + " " + e.default)"#,
                "-e",
                r#"embed("graph vector").to_string()"#,
            ],
            &[],
        );
        assert_eq!(res.code, 0, "{}", res.stderr);
        assert!(res.stdout.contains("tiny model2vec@"), "{}", res.stdout);
        assert!(res.stdout.contains(" 3 true"), "{}", res.stdout);
        assert!(
            res.stdout.contains("hash-ngram/1 hash-ngram/1 256 false"),
            "{}",
            res.stdout
        );
        // The default embedder (3 components) answered embed(text).
        let last = res.stdout.trim().lines().last().unwrap_or("").to_string();
        assert!(last.contains("vector(["), "{last}");
        assert_eq!(last.matches(',').count(), 2, "{last}");

        // Without flags only the built-in is there, and it is the default.
        let res = run_env(&["-e", r#"g.embedders().map(|e| e.name)"#], &[]);
        assert_eq!(res.code, 0, "{}", res.stderr);
        assert!(res.stdout.contains("hash-ngram/1"), "{}", res.stdout);
        assert!(!res.stdout.contains("tiny"), "{}", res.stdout);

        // The step on the model, in the explicit form.
        let res = run_env(
            &[
                "--embedder",
                &spec,
                "-e",
                r#"g.v().has_label("software").embed("name", #{embedder: "tiny"}).count().next()"#,
            ],
            &[],
        );
        assert_eq!(res.code, 0, "{}", res.stderr);
        assert_eq!(res.stdout.trim(), "2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn embedder_from_a_repository_in_the_models_dir() {
        let models = fresh_dir("m2v_models");
        let model = models.join("acme").join("tiny-model");
        write_model(&model);
        std::fs::write(
            model.join("graphersal-embed-model.json"),
            r#"{"format": 1, "repo": "acme/tiny-model", "revision": "main", "files": []}"#,
        )
        .unwrap();
        let models_text = models.display().to_string();
        let env = [("GRAPHERSAL_EMBED_MODELS", models_text.as_str())];
        let res = run_env(
            &[
                "--embedder",
                "t=acme/tiny-model",
                "-e",
                r#"g.embedders().filter(|e| e.name == "t")[0].model"#,
            ],
            &env,
        );
        assert_eq!(res.code, 0, "{}", res.stderr);
        assert!(
            res.stdout.contains("model2vec/acme/tiny-model@"),
            "{}",
            res.stdout
        );

        // A copy elsewhere (as a path) has the same identity.
        let copy = fresh_dir("m2v_copy");
        for file in [
            "config.json",
            "tokenizer.json",
            "model.safetensors",
            "graphersal-embed-model.json",
        ] {
            std::fs::copy(model.join(file), copy.join(file)).unwrap();
        }
        let spec = format!("t={}", copy.display());
        let again = run_env(
            &[
                "--embedder",
                &spec,
                "-e",
                r#"g.embedders().filter(|e| e.name == "t")[0].model"#,
            ],
            &[],
        );
        assert_eq!(again.stdout, res.stdout);

        // list shows it with that identity.
        let list = run_env(&["embed-model", "list"], &env);
        assert_eq!(list.code, 0, "{}", list.stderr);
        assert!(list.stdout.contains("acme/tiny-model"), "{}", list.stdout);
        let identity = res.stdout.trim().trim_matches('"').to_string();
        assert!(
            list.stdout.contains(&identity),
            "{} / {identity}",
            list.stdout
        );

        // An unknown repository: exit 2 with the download hint.
        let res = run_env(&["--embedder", "x=acme/missing", "-e", "1"], &env);
        assert_eq!(res.code, 2, "{}", res.stderr);
        assert!(
            res.stderr
                .contains("run `graphersal embed-model download acme/missing`"),
            "{}",
            res.stderr
        );
        assert!(res.stdout.is_empty());
        let _ = std::fs::remove_dir_all(&models);
        let _ = std::fs::remove_dir_all(&copy);
    }

    #[test]
    fn embedder_flag_errors_are_usage_errors() {
        let dir = fresh_dir("m2v_errors");
        write_model(&dir);
        let spec = format!("tiny={}", dir.display());
        let empty = fresh_dir("m2v_errors_models");
        let empty_text = empty.display().to_string();
        let reserved = format!("hash-ngram/2={}", dir.display());
        let no_model = format!("x={}", empty.display());
        let cases: Vec<(Vec<&str>, &str)> = vec![
            (vec!["--embedder", "tiny"], "Expected NAME=PATH"),
            (vec!["--embedder", "=x"], "Expected NAME=PATH"),
            (
                vec!["--embedder", &spec, "--embedder", &spec],
                "given twice",
            ),
            (vec!["--default-embedder", "nope"], "is not registered"),
            (vec!["--embedder", "x=./no/such/dir/"], "no such directory"),
            (
                vec!["--embedder", "x=acme/missing"],
                "neither a model directory",
            ),
            (vec!["--embedder", &reserved], "reserved"),
            (vec!["--embedder", &no_model], "embed-model download"),
        ];
        for (mut args, needle) in cases {
            args.extend(["-e", "1"]);
            let res = run_env(&args, &[("GRAPHERSAL_EMBED_MODELS", empty_text.as_str())]);
            assert_eq!(res.code, 2, "{args:?}: {}", res.stderr);
            assert!(res.stderr.contains(needle), "{args:?}: {}", res.stderr);
            assert!(res.stdout.is_empty(), "{args:?}: {}", res.stdout);
        }
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }

    // -----------------------------------------------------------------------------------------
    // A mock Hugging Face endpoint
    // -----------------------------------------------------------------------------------------

    /// One answer of the mock: status, headers, body, and how many body bytes are really sent
    /// (fewer than `Content-Length`: a broken-off download).
    #[derive(Clone)]
    struct Answer {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        sent: Option<usize>,
    }

    impl Answer {
        fn ok(body: Vec<u8>) -> Self {
            Answer {
                status: 200,
                headers: Vec::new(),
                body,
                sent: None,
            }
        }

        fn redirect(status: u16, location: &str) -> Self {
            Answer {
                status,
                headers: vec![("Location".to_string(), location.to_string())],
                body: Vec::new(),
                sent: None,
            }
        }

        fn header(mut self, name: &str, value: &str) -> Self {
            self.headers.push((name.to_string(), value.to_string()));
            self
        }
    }

    type Routes = HashMap<String, Answer>;

    /// The path and the Authorization header of every request the mock saw.
    type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

    /// A minimal HTTP/1.1 server on 127.0.0.1 (one request per connection): answers by path
    /// and records the path and the Authorization header of every request.
    struct Mock {
        port: u16,
        seen: Seen,
    }

    impl Mock {
        /// Serves the routes `make` builds from the mock's port.
        fn start(make: impl FnOnce(u16) -> Routes) -> Mock {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let routes = make(port);
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = seen.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let Ok(clone) = stream.try_clone() else {
                        continue;
                    };
                    let mut reader = BufReader::new(clone);
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() {
                        continue;
                    }
                    let path = line.split_whitespace().nth(1).unwrap_or("");
                    let path = path.split('?').next().unwrap_or("").to_string();
                    let mut auth = None;
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                            break;
                        }
                        if let Some((name, value)) = header.split_once(':')
                            && name.eq_ignore_ascii_case("authorization")
                        {
                            auth = Some(value.trim().to_string());
                        }
                    }
                    log.lock().unwrap().push((path.clone(), auth));
                    let answer = routes.get(&path).cloned().unwrap_or(Answer {
                        status: 404,
                        headers: Vec::new(),
                        body: b"not found".to_vec(),
                        sent: None,
                    });
                    let mut head = format!(
                        "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                        answer.status,
                        answer.body.len()
                    );
                    for (name, value) in &answer.headers {
                        head.push_str(&format!("{name}: {value}\r\n"));
                    }
                    head.push_str("\r\n");
                    let sent = answer.sent.unwrap_or(answer.body.len());
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&answer.body[..sent]);
                    let _ = stream.flush();
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
            });
            Mock { port, seen }
        }

        fn endpoint(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
        }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn sha256(bytes: &[u8]) -> String {
        hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
    }

    fn git_blob_id(bytes: &[u8]) -> String {
        let mut context = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
        context.update(format!("blob {}\0", bytes.len()).as_bytes());
        context.update(bytes);
        hex(context.finish().as_ref())
    }

    /// The routes of a repository `acme/tiny` at `main`, the Hugging Face way: config.json
    /// directly (its git blob id as the ETag), tokenizer.json through a relative 307, the
    /// safetensors file through a 302 to another host (`localhost` instead of `127.0.0.1`) with
    /// the stated SHA-256 in X-Linked-Etag.
    fn good_routes(port: u16, model: Vec<u8>, stated_sha: &str) -> Routes {
        let config = config_json();
        let tokenizer = tokenizer_json();
        let base = "/acme/tiny/resolve/main";
        let mut routes = HashMap::new();
        routes.insert(
            format!("{base}/config.json"),
            Answer::ok(config.clone())
                .header("ETag", &format!("\"{}\"", git_blob_id(&config)))
                .header("X-Repo-Commit", "0123456789abcdef0123456789abcdef01234567"),
        );
        routes.insert(
            format!("{base}/tokenizer.json"),
            Answer::redirect(
                307,
                "/api/resolve-cache/acme/tiny/c0ffee/tokenizer.json?x=1",
            )
            .header("X-Linked-Etag", &format!("\"{}\"", git_blob_id(&tokenizer))),
        );
        routes.insert(
            "/api/resolve-cache/acme/tiny/c0ffee/tokenizer.json".to_string(),
            Answer::ok(tokenizer),
        );
        routes.insert(
            format!("{base}/model.safetensors"),
            Answer::redirect(
                302,
                &format!("http://localhost:{port}/cdn/model.safetensors?sig=abc"),
            )
            .header("X-Linked-Etag", &format!("\"{stated_sha}\""))
            .header("X-Linked-Size", &model.len().to_string()),
        );
        routes.insert(
            "/cdn/model.safetensors".to_string(),
            Answer::ok(model).header("ETag", "\"cdn-etag\""),
        );
        routes
    }

    /// Staging directories and `.part` files anywhere under `models`.
    fn leftovers(models: &Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut stack = vec![models.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || name.ends_with(".part") {
                    found.push(entry.path().display().to_string());
                }
                if entry.path().is_dir() {
                    stack.push(entry.path());
                }
            }
        }
        found
    }

    #[test]
    fn download_writes_the_model_and_its_manifest() {
        let model = model_safetensors();
        let sha = sha256(&model);
        let mock = Mock::start(|port| good_routes(port, model.clone(), &sha));
        let models = fresh_dir("m2v_download");
        let models_text = models.display().to_string();
        let endpoint = mock.endpoint();
        let res = run_env(
            &[
                "embed-model",
                "download",
                "acme/tiny",
                "--dir",
                &models_text,
            ],
            &[
                ("GRAPHERSAL_HF_ENDPOINT", endpoint.as_str()),
                ("HF_TOKEN", "secret-token"),
            ],
        );
        assert_eq!(res.code, 0, "stdout {} stderr {}", res.stdout, res.stderr);
        assert!(res.stdout.contains("acme/tiny ->"), "{}", res.stdout);
        assert!(
            res.stdout.contains("identity model2vec/acme/tiny@"),
            "{}",
            res.stdout
        );
        assert!(res.stdout.contains("dimension 3"), "{}", res.stdout);
        assert!(
            res.stderr.contains("model.safetensors"),
            "progress: {}",
            res.stderr
        );
        assert!(
            !res.stderr.contains("secret-token"),
            "the token is never shown"
        );
        let dir = models.join("acme").join("tiny");
        assert_eq!(std::fs::read(dir.join("model.safetensors")).unwrap(), model);
        assert_eq!(
            std::fs::read(dir.join("tokenizer.json")).unwrap(),
            tokenizer_json()
        );
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.join("graphersal-embed-model.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["repo"], "acme/tiny");
        assert_eq!(manifest["revision"], "main");
        assert_eq!(
            manifest["commit"],
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(manifest["files"][2]["name"], "model.safetensors");
        assert_eq!(manifest["files"][2]["sha256"], sha.as_str());
        assert_eq!(manifest["files"][2]["size"], model.len());
        assert!(manifest["downloaded_at"].as_str().unwrap().ends_with('Z'));
        assert!(leftovers(&models).is_empty(), "{:?}", leftovers(&models));

        // The token went to the endpoint's host only, never to the redirect's other host.
        let seen = mock.seen.lock().unwrap().clone();
        assert!(seen.iter().any(|(path, _)| path.starts_with("/cdn/")));
        for (path, auth) in &seen {
            if path.starts_with("/cdn/") {
                assert_eq!(auth, &None, "{path}");
            } else {
                assert_eq!(auth.as_deref(), Some("Bearer secret-token"), "{path}");
            }
        }

        // list, and the model through --embedder REPO.
        let env = [("GRAPHERSAL_EMBED_MODELS", models_text.as_str())];
        let list = run_env(&["embed-model", "list"], &env);
        assert_eq!(list.code, 0, "{}", list.stderr);
        assert!(list.stdout.starts_with("REPO"), "{}", list.stdout);
        assert!(
            list.stdout.contains("model2vec/acme/tiny@"),
            "{}",
            list.stdout
        );
        let res = run_env(
            &[
                "--embedder",
                "t=acme/tiny",
                "-e",
                r#"embed("graph", #{embedder: "t"}).to_string()"#,
            ],
            &env,
        );
        assert_eq!(res.code, 0, "{}", res.stderr);
        assert!(res.stdout.contains("vector(["), "{}", res.stdout);

        // A second download replaces the first one.
        let again = run_env(
            &[
                "embed-model",
                "download",
                "acme/tiny",
                "--dir",
                &models_text,
            ],
            &[("GRAPHERSAL_HF_ENDPOINT", endpoint.as_str())],
        );
        assert_eq!(again.code, 0, "{}", again.stderr);
        assert!(dir.join("model.safetensors").is_file());
        assert!(leftovers(&models).is_empty(), "{:?}", leftovers(&models));
        let _ = std::fs::remove_dir_all(&models);
    }

    #[test]
    fn a_failed_download_leaves_no_model() {
        let model = model_safetensors();
        let good_sha = sha256(&model);
        type Make = Box<dyn FnOnce(u16) -> Routes>;
        let cases: Vec<(&str, Make, &str)> = vec![
            (
                "sha256 mismatch",
                Box::new({
                    let model = model.clone();
                    move |port| good_routes(port, model, &"0".repeat(64))
                }),
                "nothing was kept",
            ),
            (
                "git blob id mismatch",
                Box::new({
                    let (model, sha) = (model.clone(), good_sha.clone());
                    move |port| {
                        let mut routes = good_routes(port, model, &sha);
                        routes.insert(
                            "/acme/tiny/resolve/main/config.json".to_string(),
                            Answer::ok(config_json())
                                .header("ETag", &format!("\"{}\"", "1".repeat(40))),
                        );
                        routes
                    }
                }),
                "git blob id",
            ),
            (
                "broken off",
                Box::new({
                    let (model, sha) = (model.clone(), good_sha.clone());
                    move |port| {
                        let mut routes = good_routes(port, model.clone(), &sha);
                        let mut cut = Answer::ok(model.clone());
                        cut.sent = Some(model.len() / 2);
                        routes.insert("/cdn/model.safetensors".to_string(), cut);
                        routes
                    }
                }),
                "broke off",
            ),
            (
                "missing file",
                Box::new({
                    let (model, sha) = (model.clone(), good_sha.clone());
                    move |port| {
                        let mut routes = good_routes(port, model, &sha);
                        routes.remove("/acme/tiny/resolve/main/model.safetensors");
                        routes
                    }
                }),
                "has no model.safetensors",
            ),
            (
                "not a model",
                Box::new(|port| {
                    let garbage = b"not a safetensors file".to_vec();
                    let sha = sha256(&garbage);
                    good_routes(port, garbage, &sha)
                }),
                "does not load as a Model2Vec model",
            ),
            (
                "refused",
                Box::new(|_| {
                    let mut refused = Answer::ok(b"no".to_vec());
                    refused.status = 401;
                    HashMap::from([("/acme/tiny/resolve/main/config.json".to_string(), refused)])
                }),
                "HF_TOKEN",
            ),
        ];
        for (what, make, needle) in cases {
            let mock = Mock::start(make);
            let models = fresh_dir("m2v_failures");
            let models_text = models.display().to_string();
            let endpoint = mock.endpoint();
            let res = run_env(
                &[
                    "embed-model",
                    "download",
                    "acme/tiny",
                    "--dir",
                    &models_text,
                ],
                &[("GRAPHERSAL_HF_ENDPOINT", endpoint.as_str())],
            );
            assert_eq!(res.code, 1, "{what}: {} {}", res.stdout, res.stderr);
            assert!(res.stderr.contains(needle), "{what}: {}", res.stderr);
            assert!(res.stdout.is_empty(), "{what}: {}", res.stdout);
            assert!(
                !models.join("acme").join("tiny").exists(),
                "{what}: no model left"
            );
            assert!(
                leftovers(&models).is_empty(),
                "{what}: {:?}",
                leftovers(&models)
            );
            let _ = std::fs::remove_dir_all(&models);
        }
    }

    #[test]
    fn embed_model_usage() {
        let res = run_env(&["embed-model"], &[]);
        assert_eq!(res.code, 2);
        assert!(
            res.stdout.contains("embed-model download"),
            "{}",
            res.stdout
        );
        let res = run_env(&["embed-model", "help"], &[]);
        assert_eq!(res.code, 0);
        assert!(res.stdout.contains("embed-model list"), "{}", res.stdout);
        let res = run_env(&["embed-model", "download"], &[]);
        assert_eq!(res.code, 2, "{}", res.stderr);
        let res = run_env(&["embed-model", "download", "../../etc", "--dir", "x"], &[]);
        assert_eq!(res.code, 2, "{}", res.stderr);
        assert!(
            res.stderr.contains("not a Hugging Face repository"),
            "{}",
            res.stderr
        );
        let res = run_env(
            &[
                "embed-model",
                "download",
                "a/b",
                "--revision",
                "../x",
                "--dir",
                "x",
            ],
            &[],
        );
        assert_eq!(res.code, 2, "{}", res.stderr);
        let res = run_env(&["embed-model", "nope"], &[]);
        assert_eq!(res.code, 2, "{}", res.stderr);
        let res = run_env(&["embed-model", "list", "--bogus", "1"], &[]);
        assert_eq!(res.code, 2, "{}", res.stderr);
        let empty = fresh_dir("m2v_list_empty");
        let empty_text = empty.display().to_string();
        let res = run_env(&["embed-model", "list", "--dir", &empty_text], &[]);
        assert_eq!(res.code, 0, "{}", res.stderr);
        assert!(res.stdout.is_empty(), "{}", res.stdout);
        assert!(
            res.stderr.contains("No downloaded models"),
            "{}",
            res.stderr
        );
        let _ = std::fs::remove_dir_all(&empty);
    }

    // -----------------------------------------------------------------------------------------
    // Models inside a Store (format spec 4.5)
    // -----------------------------------------------------------------------------------------

    /// `good_routes` plus the Hub metadata (licence MIT, a base model, the file list), the
    /// model card and a NOTICE file; the base model's own metadata says Apache-2.0.
    fn routes_with_metadata(port: u16, model: Vec<u8>, gated: bool) -> Routes {
        let sha = sha256(&model);
        let mut routes = good_routes(port, model, &sha);
        let api = serde_json::json!({
            "id": "acme/tiny",
            "gated": if gated { serde_json::json!("manual") } else { serde_json::json!(false) },
            "cardData": {"license": "mit", "base_model": "acme/teacher"},
            "siblings": [
                {"rfilename": "README.md"}, {"rfilename": "NOTICE"},
                {"rfilename": "config.json"}, {"rfilename": "tokenizer.json"},
                {"rfilename": "model.safetensors"}, {"rfilename": "docs/LICENSE"}
            ]
        });
        routes.insert(
            "/api/models/acme/tiny/revision/main".to_string(),
            Answer::ok(api.to_string().into_bytes()),
        );
        routes.insert(
            "/api/models/acme/teacher".to_string(),
            Answer::ok(br#"{"cardData": {"license": "apache-2.0"}}"#.to_vec()),
        );
        routes.insert(
            "/acme/tiny/resolve/main/README.md".to_string(),
            Answer::ok(b"---\nlicense: mit\n---\n# tiny\n".to_vec()),
        );
        routes.insert(
            "/acme/tiny/resolve/main/NOTICE".to_string(),
            Answer::ok(b"tiny, by acme".to_vec()),
        );
        routes
    }

    fn create_store(path: &Path) {
        let path = path.display().to_string();
        let res = run_env(&["store", "create", &path], &[]);
        assert_eq!(res.code, 0, "{}", res.stderr);
    }

    #[test]
    fn download_into_a_store_keeps_the_licence_with_the_model() {
        let mock = Mock::start(|port| routes_with_metadata(port, model_safetensors(), false));
        let base = fresh_dir("m2v_into");
        let models = base.join("models");
        let store = base.join("store");
        create_store(&store);
        let (models_text, store_text) = (models.display().to_string(), store.display().to_string());
        let endpoint = mock.endpoint();
        let res = run_env(
            &[
                "embed-model",
                "download",
                "acme/tiny",
                "--dir",
                &models_text,
                "--into",
                &store_text,
            ],
            &[("GRAPHERSAL_HF_ENDPOINT", endpoint.as_str())],
        );
        assert_eq!(res.code, 0, "{} {}", res.stdout, res.stderr);
        assert!(res.stdout.contains("-> the store"), "{}", res.stdout);
        assert!(
            res.stdout.contains("identity model2vec/acme/tiny@"),
            "{}",
            res.stdout
        );
        // MIT without a licence file: the standard text is written beside the model.
        assert!(res.stderr.contains("standard mit text"), "{}", res.stderr);
        assert!(!res.stderr.contains("warning"), "{}", res.stderr);
        // Nothing in the models directory.
        assert!(!models.join("acme").join("tiny").exists());
        assert!(leftovers(&models).is_empty(), "{:?}", leftovers(&models));

        let listed = run_env(&["store", "models", &store_text], &[]);
        assert_eq!(listed.code, 0, "{}", listed.stderr);
        assert!(
            listed.stdout.contains("model2vec/acme/tiny@"),
            "{}",
            listed.stdout
        );
        assert!(
            listed.stdout.contains("licence mit (standard text"),
            "{}",
            listed.stdout
        );
        assert!(
            listed.stdout.contains("not used by the schema"),
            "{}",
            listed.stdout
        );
        let models_dir = std::fs::read_dir(store.join("models"))
            .unwrap()
            .flatten()
            .next()
            .unwrap()
            .path();
        for file in ["README.md", "NOTICE", "LICENSE", "model.safetensors"] {
            assert!(models_dir.join(file).is_file(), "{file}");
        }
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(models_dir.join("graphersal-embed-model.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["license"], "mit");
        assert_eq!(manifest["license_text"], "template");
        assert_eq!(manifest["base_models"][0]["repo"], "acme/teacher");
        assert_eq!(manifest["base_models"][0]["license"], "apache-2.0");
        assert_eq!(manifest["gated"], false);

        let info = run_env(&["store", "info", &store_text], &[]);
        assert!(info.stdout.contains("models      1"), "{}", info.stdout);
        assert!(info.stdout.contains("licence mit"), "{}", info.stdout);

        // The shareable copies name the models they carry, or leave them out.
        let gsnap = base.join("x.gsnap").display().to_string();
        let res = run_env(&["store", "export", &store_text, &gsnap], &[]);
        assert_eq!(res.code, 0, "{}", res.stderr);
        assert!(res.stdout.contains("embedding model(s)"), "{}", res.stdout);
        let bare = base.join("bare.gsnap").display().to_string();
        let res = run_env(
            &["store", "export", &store_text, &bare, "--without-models"],
            &[],
        );
        assert_eq!(res.code, 0, "{}", res.stderr);
        assert!(!res.stdout.contains("embedding model"), "{}", res.stdout);
        assert!(std::fs::metadata(&bare).unwrap().len() < std::fs::metadata(&gsnap).unwrap().len());
        let zip = base.join("b.zip").display().to_string();
        let res = run_env(&["store", "backup", &store_text, &zip, "--zip"], &[]);
        assert_eq!(res.code, 0, "{}", res.stderr);
        assert!(res.stdout.contains("licence mit"), "{}", res.stdout);
        let bare_zip = base.join("bare.zip").display().to_string();
        let res = run_env(
            &[
                "store",
                "backup",
                &store_text,
                &bare_zip,
                "--zip",
                "--without-models",
            ],
            &[],
        );
        assert_eq!(res.code, 0, "{}", res.stderr);
        assert!(!res.stdout.contains("licence"), "{}", res.stdout);
        let res = run_env(
            &[
                "store",
                "backup",
                &store_text,
                &base.join("dir").display().to_string(),
                "--without-models",
            ],
            &[],
        );
        assert_eq!(res.code, 2, "{}", res.stderr);
        // A store from the packed file has the model again.
        let copy = base.join("copy").display().to_string();
        let res = run_env(&["store", "create", &copy, "--from", &gsnap], &[]);
        assert_eq!(res.code, 0, "{}", res.stderr);
        let listed = run_env(&["store", "models", &copy], &[]);
        assert!(
            listed.stdout.contains("model2vec/acme/tiny@"),
            "{}",
            listed.stdout
        );

        // Removing it.
        let identity = listed
            .stdout
            .split_whitespace()
            .find(|word| word.starts_with("model2vec/"))
            .unwrap()
            .to_string();
        let res = run_env(&["store", "models", "remove", &copy, &identity], &[]);
        assert_eq!(res.code, 0, "{}", res.stderr);
        let res = run_env(&["store", "models", "remove", &copy, &identity], &[]);
        assert_eq!(res.code, 1, "{}", res.stderr);
        assert!(res.stderr.contains("store models"), "{}", res.stderr);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_gated_repository_and_an_unknown_licence_are_warned_about() {
        let mock = Mock::start(|port| routes_with_metadata(port, model_safetensors(), true));
        let base = fresh_dir("m2v_gated");
        let store = base.join("store");
        create_store(&store);
        let endpoint = mock.endpoint();
        let res = run_env(
            &[
                "embed-model",
                "download",
                "acme/tiny",
                "--dir",
                &base.join("models").display().to_string(),
                "--into",
                &store.display().to_string(),
            ],
            &[("GRAPHERSAL_HF_ENDPOINT", endpoint.as_str())],
        );
        assert_eq!(res.code, 0, "{}", res.stderr);
        assert!(res.stderr.contains("GATED"), "{}", res.stderr);
        // Without metadata (the plain mock): the licence is unknown.
        let model = model_safetensors();
        let sha = sha256(&model);
        let plain = Mock::start(|port| good_routes(port, model.clone(), &sha));
        let endpoint = plain.endpoint();
        let models = base.join("plain").display().to_string();
        let res = run_env(
            &["embed-model", "download", "acme/tiny", "--dir", &models],
            &[("GRAPHERSAL_HF_ENDPOINT", endpoint.as_str())],
        );
        assert_eq!(res.code, 0, "{}", res.stderr);
        assert!(
            res.stderr.contains("licence of acme/tiny is unknown"),
            "{}",
            res.stderr
        );
        let list = run_env(&["embed-model", "list", "--dir", &models], &[]);
        assert!(list.stdout.contains("LICENCE"), "{}", list.stdout);
        assert!(list.stdout.contains("unknown"), "{}", list.stdout);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_imported_model_the_schema_names_is_loaded_when_the_store_opens() {
        let base = fresh_dir("m2v_autoload");
        let model = base.join("model");
        write_model(&model);
        // A download's manifest names the repository: the identity is model2vec/acme/tiny@...
        std::fs::write(
            model.join("graphersal-embed-model.json"),
            r#"{"format": 1, "repo": "acme/tiny", "revision": "main", "files": []}"#,
        )
        .unwrap();
        let store = base.join("store");
        create_store(&store);
        let (model_text, store_text) = (model.display().to_string(), store.display().to_string());
        let res = run_env(
            &[
                "embed-model",
                "import",
                &model_text,
                "--into",
                &store_text,
                "--repo",
                "acme/tiny",
                "--license",
                "apache-2.0",
            ],
            &[],
        );
        assert_eq!(res.code, 0, "{} {}", res.stdout, res.stderr);
        assert!(res.stdout.contains("licence apache-2.0"), "{}", res.stdout);
        let res = run_env(&["embed-model", "import", &model_text], &[]);
        assert_eq!(res.code, 2, "{}", res.stderr);

        // Vectors made with the model through --embedder record its identity in the schema.
        let spec = format!("t={model_text}");
        let res = run_env(
            &[
                "--graph",
                &store_text,
                "--embedder",
                &spec,
                "-e",
                r#"g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object", properties: #{text: #{type: "string"}}}}}})"#,
                "-e",
                r#"g.add_v("doc").property("text", "graph vector").to_list()"#,
                "-e",
                r#"g.embed_property(#{label: "doc", from: "text", to: "emb", embedder: "t"})"#,
            ],
            &[],
        );
        assert_eq!(res.code, 0, "{} {}", res.stdout, res.stderr);
        // Opened again WITHOUT --embedder: the stored model serves the text query.
        let res = run_env(
            &[
                "--graph",
                &store_text,
                "-e",
                r#"g.v().has_label("doc").nearest("emb", "graph", 1).count().next()"#,
            ],
            &[],
        );
        assert_eq!(res.code, 0, "{} {}", res.stdout, res.stderr);
        assert_eq!(res.stdout.trim(), "1");
        assert!(
            res.stderr
                .contains("loaded the embedding model model2vec/acme/tiny@"),
            "{}",
            res.stderr
        );
        let info = run_env(&["store", "info", &store_text], &[]);
        assert!(
            info.stdout.contains("used by the schema"),
            "{}",
            info.stdout
        );

        // An explicit --embedder of the same model wins: nothing is loaded from the store.
        let res = run_env(
            &["--graph", &store_text, "--embedder", &spec, "-e", "1"],
            &[],
        );
        assert_eq!(res.code, 0, "{}", res.stderr);
        assert!(
            !res.stderr.contains("loaded the embedding model"),
            "{}",
            res.stderr
        );

        // Without the model in the store: a note naming the command that adds it.
        let listed = run_env(&["store", "models", &store_text], &[]);
        let identity = listed
            .stdout
            .split_whitespace()
            .find(|word| word.starts_with("model2vec/"))
            .unwrap()
            .to_string();
        let res = run_env(&["store", "models", "remove", &store_text, &identity], &[]);
        assert_eq!(res.code, 0, "{}", res.stderr);
        let res = run_env(&["--graph", &store_text, "-e", "1"], &[]);
        assert_eq!(res.code, 0, "{}", res.stderr);
        assert!(
            res.stderr
                .contains("graphersal embed-model download acme/tiny --into"),
            "{}",
            res.stderr
        );
        let info = run_env(&["store", "info", &store_text], &[]);
        assert!(info.stdout.contains("missing"), "{}", info.stdout);
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[test]
fn an_args_file_supplies_flags_and_the_command_line_wins() {
    let dir = temp_path("argsfile");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("a.json");
    std::fs::write(
        &file,
        r#"{"_c": "comment", "graph": "empty", "eval": ["g.v().count().next()"], "format": "json"}"#,
    )
    .unwrap();
    let path = file.to_str().unwrap();

    let res = run(&["--args-file", path], "");
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(res.stdout.trim(), "0");

    // @FILE, the graph replaced on the command line, eval scripts concatenated (file first)
    let res = run(
        &[
            &format!("@{path}"),
            "--graph",
            "modern",
            "-e",
            "g.v().count().next()",
        ],
        "",
    );
    assert_eq!(res.code, 0, "stderr: {}", res.stderr);
    assert_eq!(
        res.stdout.lines().map(str::trim).collect::<Vec<_>>(),
        ["6", "6"]
    );

    for (content, needle) in [
        (r#"{"grpah": "x"}"#, "did you mean \"graph\"?"),
        (r#"{"args-file": "b.json"}"#, "another"),
        (r#"{"port": true}"#, "key \"port\""),
        ("not json", "not valid JSON"),
    ] {
        std::fs::write(&file, content).unwrap();
        let res = run(&["--args-file", path], "");
        assert_eq!(res.code, 2, "{content}");
        assert!(res.stderr.contains(needle), "{}", res.stderr);
        assert!(res.stderr.contains("graphersal --help"), "{}", res.stderr);
    }
    let res = run(&["--args-file", "/nonexistent/args.json"], "");
    assert_eq!(res.code, 2);
    assert!(res.stderr.contains("cannot read"), "{}", res.stderr);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn mcp_instructions_need_mcp_and_a_readable_file() {
    let res = run(&["--server", "--mcp-instructions", "/nonexistent/n.md"], "");
    assert_eq!(res.code, 2);
    assert!(res.stderr.contains("--mcp-instructions"), "{}", res.stderr);
    let res = run(
        &[
            "--server",
            "--mcp",
            "--mcp-instructions",
            "/nonexistent/n.md",
        ],
        "",
    );
    assert_eq!(res.code, 2);
    assert!(res.stderr.contains("cannot read"), "{}", res.stderr);
}
