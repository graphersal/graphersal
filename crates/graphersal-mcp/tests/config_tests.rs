//! The operator's configuration (`McpConfig`, `--mcp-config`): parsing and its errors, what a
//! configured endpoint lists and refuses, short descriptions, and the prompt budgets (a long
//! new description shows here).

use std::sync::Mutex;
use std::sync::mpsc::{Sender, channel};

use graphersal_mcp::{
    ClientRef, Executor, Host, Job, Mcp, McpConfig, McpError, McpRequest, PromptCost,
};
use graphersal_session::Session;
use serde_json::{Value, json};

/// A host over one session on its own thread (the modern graph, plus `setup` scripts).
struct TestHost {
    jobs: Mutex<Sender<Job>>,
}

impl TestHost {
    fn modern() -> Self {
        let (jobs, queue) = channel::<Job>();
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                let mut session = Session::new();
                session
                    .load_sample("modern")
                    .expect("the modern graph loads");
                for job in queue {
                    job(&mut session);
                }
            })
            .expect("the session thread starts");
        Self {
            jobs: Mutex::new(jobs),
        }
    }

    /// The modern graph with three saved queries: `older` (folder kb), `deep` (folder kb/sub),
    /// `other` (folder misc).
    fn with_queries() -> Self {
        let host = Self::modern();
        let mcp = Mcp::new(false, None, false);
        let script = "g.define_query(#{name: \"older\", folder: \"kb\", description: \"People \
                      older than min.\", params: #{min: #{schema: #{type: \"integer\"}, default: \
                      30}}, body: \"fn older(min) { g.v().has(\\\"age\\\", P.gt(min)).values(\\\"name\\\") }\"}); \
                      g.define_query(#{name: \"deep\", folder: \"kb/sub\", body: \"fn deep() { 1 }\"}); \
                      g.define_query(#{name: \"other\", folder: \"misc\", body: \"fn other() { 2 }\"}); 0";
        let answer = call(&mcp, &host, "query", json!({"script": script}));
        assert!(!answer["isError"].as_bool().unwrap_or(true), "{answer}");
        host
    }
}

impl Executor for TestHost {
    fn execute(&self, _client: &ClientRef, job: Job) -> Result<(), McpError> {
        let (done, wait) = channel();
        self.jobs
            .lock()
            .expect("jobs")
            .send(Box::new(move |session| {
                job(session);
                let _ = done.send(());
            }))
            .map_err(|_| McpError::Unavailable("the session is not running".into()))?;
        let _ = wait.recv();
        Ok(())
    }
}

impl Host for TestHost {
    fn view(&self) -> Value {
        Value::Null
    }

    fn add_process_statistics(&self, statistics: Value) -> Value {
        statistics
    }

    fn add_graph_info(&self, _info: &mut Value) -> Result<(), McpError> {
        Ok(())
    }

    fn examples_json(&self) -> Option<Vec<u8>> {
        None
    }
}

fn rpc(mcp: &Mcp, host: &TestHost, method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let request = McpRequest::new("POST", body.as_bytes())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    let response = mcp.handle(host, request);
    serde_json::from_slice(response.body()).expect("a JSON body")
}

/// `tools/call` of `name`: the JSON-RPC `result`, or the whole answer on an error.
fn call(mcp: &Mcp, host: &TestHost, name: &str, args: Value) -> Value {
    let answer = rpc(
        mcp,
        host,
        "tools/call",
        json!({"name": name, "arguments": args}),
    );
    answer.get("result").cloned().unwrap_or(answer)
}

fn parse(text: &str) -> Result<McpConfig, String> {
    McpConfig::parse(text, &mut |name| match name {
        "notes.md" => Ok("Notes from a file.".into()),
        _ => Err("no such file".into()),
    })
    .map_err(|err| err.to_string())
}

fn names(listed: &[Value]) -> Vec<&str> {
    listed.iter().filter_map(|t| t["name"].as_str()).collect()
}

/// The knowledge-graph example of `graphersal-cli`.
const KB_EXAMPLE: &str = include_str!("../../graphersal-cli/examples/mcp-kb-readonly.json");

#[test]
fn an_empty_configuration_is_the_endpoint_without_one() {
    let host = TestHost::modern();
    let config = parse(r#"{"_comment": "nothing"}"#).expect("parses");
    assert_eq!(config, McpConfig::default());
    for read_only in [false, true] {
        let plain = Mcp::new(read_only, None, false);
        let configured = Mcp::new(read_only, None, false).with_config(config.clone());
        assert_eq!(
            plain.tool_listings(&host).expect("lists"),
            configured.tool_listings(&host).expect("lists")
        );
        assert_eq!(plain.instructions_text(), configured.instructions_text());
        assert_eq!(
            rpc(&plain, &host, "resources/list", json!({})),
            rpc(&configured, &host, "resources/list", json!({}))
        );
    }
}

#[test]
fn unknown_keys_and_names_are_errors_with_a_hint() {
    let err = parse(r#"{"descriptons": "short"}"#).unwrap_err();
    assert!(
        err.contains("\"descriptons\"") && err.contains("did you mean \"descriptions\""),
        "{err}"
    );
    let err = parse(r#"{"tools": {"includ": []}}"#).unwrap_err();
    assert!(err.contains("did you mean \"tools.include\""), "{err}");
    let err = parse(r#"{"tools": {"include": ["gq1"]}}"#).unwrap_err();
    assert!(
        err.contains("\"gq1\"") && err.contains("did you mean \"gql\""),
        "{err}"
    );
    let err = parse(r#"{"tools": {"overrides": {"nope": {"description": "x"}}}}"#).unwrap_err();
    assert!(err.contains("tools.overrides"), "{err}");
    let err = parse(r#"{"resources": ["schemas"]}"#).unwrap_err();
    assert!(err.contains("did you mean \"schema\""), "{err}");
    let err = parse(r#"{"descriptions": "tiny"}"#).unwrap_err();
    assert!(err.contains("the only value is \"short\""), "{err}");
    let err = parse(r#"{"answer_limit_chars": 10}"#).unwrap_err();
    assert!(err.contains("at least 1000"), "{err}");
    let err = parse(r#"{"saved_query_tools": {"paging_param": false}}"#).unwrap_err();
    assert!(err.contains("saved_query_tools.paging_params"), "{err}");
    assert!(parse("[]").unwrap_err().contains("JSON object"));
    assert!(parse("{").unwrap_err().contains("not valid JSON"));
    // `_` keys are comments at every level.
    parse(
        r#"{"_a": 1, "tools": {"_b": 2, "overrides": {"_c": 3}}, "saved_query_tools": {"_d": 4}}"#,
    )
    .expect("comments are ignored");
}

#[test]
fn instructions_modes_and_their_errors() {
    let host = TestHost::modern();
    let replaced =
        parse(r#"{"instructions": {"mode": "replace", "text": " Only this. "}}"#).expect("parses");
    let mcp = Mcp::new(true, None, false).with_config(replaced);
    assert_eq!(mcp.instructions_text(), "Only this.");
    let init = rpc(
        &mcp,
        &host,
        "initialize",
        json!({"protocolVersion": "2025-06-18"}),
    );
    assert_eq!(init["result"]["instructions"], "Only this.");

    let appended =
        parse(r#"{"instructions": {"mode": "append", "file": "notes.md"}}"#).expect("parses");
    let flag = Mcp::new(false, None, false).with_operator_notes(Some("Notes from a file.".into()));
    assert_eq!(
        Mcp::new(false, None, false)
            .with_config(appended)
            .instructions_text(),
        flag.instructions_text()
    );

    for (text, wanted) in [
        (
            r#"{"instructions": {"mode": "builtin", "text": "x"}}"#,
            "takes no text",
        ),
        (
            r#"{"instructions": {"mode": "replace"}}"#,
            "needs \"text\" or \"file\"",
        ),
        (
            r#"{"instructions": {"mode": "append", "text": "a", "file": "b"}}"#,
            "not both",
        ),
        (
            r#"{"instructions": {"mode": "other", "text": "a"}}"#,
            "\"builtin\", \"append\" or \"replace\"",
        ),
        (
            r#"{"instructions": {"mode": "append", "file": "missing.md"}}"#,
            "missing.md: no such file",
        ),
        (
            r#"{"instructions": {"mode": "append", "text": "   "}}"#,
            "expected 1..20000",
        ),
    ] {
        let err = parse(text).unwrap_err();
        assert!(err.contains(wanted), "{text}: {err}");
    }
}

#[test]
fn a_flag_and_the_key_of_the_same_setting_are_an_error() {
    let config = parse(r#"{"instructions": {"mode": "replace", "text": "x"}, "saved_query_tools": {}, "answer_limit_chars": 2000}"#)
        .expect("parses");
    let err = config
        .clone()
        .with_flags(Some("n".into()), false, None)
        .unwrap_err();
    assert!(err.to_string().contains("--mcp-instructions"), "{err}");
    let err = config.clone().with_flags(None, true, None).unwrap_err();
    assert!(err.to_string().contains("--mcp-query-tools"), "{err}");
    let err = config.with_flags(None, false, Some(5000)).unwrap_err();
    assert!(err.to_string().contains("--mcp-max-chars"), "{err}");
    let folded = McpConfig::default()
        .with_flags(Some("n".into()), true, Some(5000))
        .expect("no conflict");
    assert!(folded.has_saved_query_tools());
    assert_eq!(folded.answer_limit_chars(), Some(5000));
}

#[test]
fn a_writing_tool_named_on_a_read_only_server_is_an_error() {
    let config = parse(r#"{"tools": {"include": ["gql", "set_schema"]}}"#).expect("parses");
    config.check(false).expect("a read-write server offers it");
    let err = config.check(true).unwrap_err().to_string();
    assert!(
        err.contains("\"set_schema\"") && err.contains("read-only"),
        "{err}"
    );
    let config =
        parse(r#"{"tools": {"overrides": {"drop_index": {"description": "x"}}}}"#).expect("parses");
    assert!(config.check(true).is_err());
    // Excluding a writing tool is fine either way; gql runs read-only there.
    parse(r#"{"tools": {"include": ["gql", "query", "run_saved_query"], "exclude": []}}"#)
        .expect("parses")
        .check(true)
        .expect("reading tools");
    let err = parse(r#"{"tools": {"include": ["gql"], "exclude": ["gql"]}}"#).unwrap_err();
    assert!(err.contains("both"), "{err}");
    let err =
        parse(r#"{"tools": {"include": ["gql"], "overrides": {"query": {"description": "x"}}}}"#)
            .unwrap_err();
    assert!(err.contains("not offered"), "{err}");
}

#[test]
fn a_configured_endpoint_lists_and_runs_only_its_tools() {
    let host = TestHost::with_queries();
    let config = parse(
        r#"{"server_name": "project-kb",
            "tools": {"include": ["gql", "get_schema", "run_saved_query"],
                      "overrides": {"get_schema": {"description": "The schema."}}},
            "saved_query_tools": {"folders": ["kb"], "exclude": ["deep"]},
            "resources": ["schema", "gql-reference"]}"#,
    )
    .expect("parses");
    let mcp = Mcp::new(true, None, false).with_config(config);
    let init = rpc(
        &mcp,
        &host,
        "initialize",
        json!({"protocolVersion": "2025-06-18"}),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "project-kb");

    let listed = mcp.tool_listings(&host).expect("lists");
    assert_eq!(
        names(&listed),
        ["gql", "run_saved_query", "get_schema", "older"]
    );
    assert_eq!(listed[2]["description"], "The schema.");
    let older = &listed[3];
    // The query's own description, without a folder tail; no paging parameters listed.
    assert_eq!(older["description"], "People older than min.", "{older}");
    assert!(older["inputSchema"]["properties"].get("max_rows").is_none());

    let answer = call(&mcp, &host, "older", json!({"min": 30}));
    assert_eq!(answer["isError"], false, "{answer}");
    assert!(
        answer["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("josh"),
        "{answer}"
    );
    // Not offered: a built-in tool left out, a saved query outside the selection.
    let refused = call(&mcp, &host, "statistics", json!({}));
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not offered by this server's configuration"),
        "{refused}"
    );
    let refused = call(&mcp, &host, "other", json!({}));
    assert_eq!(
        refused["error"]["message"], "unknown tool \"other\"",
        "{refused}"
    );
    let refused = call(&mcp, &host, "deep", json!({}));
    assert!(refused.get("error").is_some(), "{refused}");
    // run_saved_query still reaches every saved query.
    let answer = call(&mcp, &host, "run_saved_query", json!({"name": "other"}));
    assert_eq!(answer["isError"], false, "{answer}");

    let resources = rpc(&mcp, &host, "resources/list", json!({}));
    let uris: Vec<&str> = resources["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["uri"].as_str())
        .collect();
    assert_eq!(uris, ["graphersal://schema", "graphersal://gql-reference"]);
}

#[test]
fn saved_query_selection_by_folder_and_name() {
    let host = TestHost::with_queries();
    for (config, wanted) in [
        (
            r#"{"saved_query_tools": {}}"#,
            vec!["older", "deep", "other"],
        ),
        (
            r#"{"saved_query_tools": {"folders": ["kb"]}}"#,
            vec!["older", "deep"],
        ),
        (
            r#"{"saved_query_tools": {"folders": ["kb/sub"]}}"#,
            vec!["deep"],
        ),
        (r#"{"saved_query_tools": {"folders": ["k"]}}"#, vec![]),
        (
            r#"{"saved_query_tools": {"names": ["other"]}}"#,
            vec!["other"],
        ),
        (
            r#"{"saved_query_tools": {"folders": ["misc"], "names": ["older"]}}"#,
            vec!["older", "other"],
        ),
    ] {
        let mcp = Mcp::new(false, None, false).with_config(parse(config).expect("parses"));
        let listed = mcp.tool_listings(&host).expect("lists");
        let mut got: Vec<&str> = names(&listed).into_iter().skip(27).collect();
        let mut wanted = wanted;
        got.sort_unstable();
        wanted.sort_unstable();
        assert_eq!(got, wanted, "{config}");
    }
}

#[test]
fn descriptions_are_short_and_saved_query_tools_carry_their_own_text() {
    let host = TestHost::with_queries();
    let listed = Mcp::new(false, None, false)
        .with_config(parse(r#"{"saved_query_tools": {}}"#).expect("parses"))
        .tool_listings(&host)
        .expect("lists");
    for tool in listed.iter().take(27) {
        let text = tool["description"].as_str().unwrap();
        // gql and query also carry what writing correctly needs (element ids, the write
        // clauses and steps, one transaction).
        let limit = if matches!(tool["name"].as_str(), Some("gql" | "query")) {
            2_000
        } else {
            520
        };
        assert!(
            !text.is_empty() && text.len() < limit,
            "{}: {text}",
            tool["name"]
        );
    }
    let gql = listed.iter().find(|t| t["name"] == "gql").unwrap();
    assert!(
        gql["description"]
            .as_str()
            .unwrap()
            .contains("graphersal://gql-reference")
    );
    let older = listed.iter().find(|t| t["name"] == "older").unwrap();
    assert_eq!(older["description"], "People older than min.");
    assert_eq!(
        older["inputSchema"]["properties"],
        json!({"min": {"type": "integer", "default": 30}})
    );
    // The paging parameters are listed only with paging_params: true.
    for (config, paging) in [
        (r#"{"saved_query_tools": {"paging_params": true}}"#, true),
        (r#"{"saved_query_tools": {"paging_params": false}}"#, false),
        (r#"{"saved_query_tools": {}}"#, false),
    ] {
        let listed = Mcp::new(false, None, false)
            .with_config(parse(config).expect("parses"))
            .tool_listings(&host)
            .expect("lists");
        let older = listed.iter().find(|t| t["name"] == "older").unwrap();
        assert_eq!(
            older["inputSchema"]["properties"].get("offset").is_some(),
            paging
        );
    }
    // Paging arguments still work without being listed.
    let mcp =
        Mcp::new(false, None, false).with_config(parse(r#"{"saved_query_tools": {}}"#).unwrap());
    let answer = call(&mcp, &host, "older", json!({"min": 0, "max_rows": 1}));
    assert!(
        answer["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("next page offset"),
        "{answer}"
    );
}

#[test]
fn descriptions_short_is_accepted_and_full_was_removed() {
    // "short" (the only style) is a no-op, so existing files keep working.
    assert_eq!(
        parse(r#"{"descriptions": "short"}"#).expect("parses"),
        McpConfig::default()
    );
    let err = parse(r#"{"descriptions": "full"}"#).unwrap_err();
    assert!(
        err.contains("\"full\" was removed") && err.contains("graphersal://gql-reference"),
        "{err}"
    );
}

#[test]
fn the_gql_and_query_texts_name_the_writes_only_where_writes_run() {
    let host = TestHost::modern();
    let description = |read_only: bool, name: &str| -> String {
        Mcp::new(read_only, None, false)
            .tool_listings(&host)
            .expect("lists")
            .iter()
            .find(|t| t["name"] == name)
            .and_then(|t| t["description"].as_str())
            .expect("listed")
            .to_string()
    };
    for read_only in [false, true] {
        let gql = description(read_only, "gql");
        // What the A/B test found missing: element ids, anchoring variable-length patterns.
        assert!(
            gql.contains("element_id(n)") && gql.contains("not id(n)"),
            "{gql}"
        );
        assert!(gql.contains("Anchor a variable-length pattern"), "{gql}");
        assert!(gql.contains("graphersal://gql-reference"), "{gql}");
        let query = description(read_only, "query");
        assert!(query.contains(".iterate()"), "{query}");
        for (text, writes) in [(&gql, "DETACH DELETE"), (&query, "add_v")] {
            assert_eq!(text.contains(writes), !read_only, "{text}");
            assert_eq!(text.contains("one transaction"), !read_only, "{text}");
        }
    }
    let gql = description(false, "gql");
    for clause in [
        "INSERT",
        "SET n.k",
        "REMOVE",
        "MERGE",
        "ON CREATE SET",
        "FOR x IN $list",
    ] {
        assert!(gql.contains(clause), "{clause}: {gql}");
    }
}

#[test]
fn the_gql_reference_is_listed_and_readable() {
    let host = TestHost::modern();
    let mcp = Mcp::new(false, None, false);
    let listed = rpc(&mcp, &host, "resources/list", json!({}));
    let uris: Vec<&str> = listed["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["uri"].as_str())
        .collect();
    assert_eq!(uris.len(), 7);
    assert!(uris.contains(&"graphersal://gql-reference"), "{uris:?}");
    let read = rpc(
        &mcp,
        &host,
        "resources/read",
        json!({"uri": "graphersal://gql-reference"}),
    );
    let text = read["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("# GQL reference\n\n") && text.contains("Supported: MATCH"),
        "{read}"
    );
}

#[test]
fn the_schema_tools_answer_the_summary_and_the_json_on_request() {
    let host = TestHost::modern();
    let mcp = Mcp::new(true, None, false);
    let text = |answer: &Value| {
        answer["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    for tool in ["get_schema", "infer_schema"] {
        let summary = call(&mcp, &host, tool, json!({}));
        let summary = text(&summary);
        assert!(
            summary.starts_with("graph: 6 vertices, 6 edges; 2 vertex labels, 2 edge labels"),
            "{tool}: {summary}"
        );
        assert!(
            summary.contains("\nperson 4: age int, name str\n"),
            "{summary}"
        );
        assert!(
            summary.contains("\ncreated 4 person->software: weight float"),
            "{summary}"
        );
        let detail = text(&call(&mcp, &host, tool, json!({"labels": ["person"]})));
        assert!(
            detail.contains("edges out: created->software 4, knows->person 2"),
            "{detail}"
        );
    }
    let stored = text(&call(&mcp, &host, "get_schema", json!({"format": "json"})));
    assert!(
        stored.starts_with("null: the graph stores no schema"),
        "{stored}"
    );
    let inferred = text(&call(
        &mcp,
        &host,
        "infer_schema",
        json!({"format": "json", "labels": ["knows"]}),
    ));
    let inferred: Value = serde_json::from_str(&inferred).expect("JSON");
    assert_eq!(inferred["vertices"], json!({}), "{inferred}");
    assert!(inferred["edges"]["knows"].is_object(), "{inferred}");
    let wrong = call(&mcp, &host, "get_schema", json!({"format": "yaml"}));
    assert_eq!(wrong["isError"], true, "{wrong}");
    // The same text as a resource (listed by default beside graphersal://schema).
    let read = rpc(
        &mcp,
        &host,
        "resources/read",
        json!({"uri": "graphersal://schema-summary"}),
    );
    let resource = read["result"]["contents"][0]["text"].as_str().unwrap_or("");
    assert!(
        resource.contains("knows 2 person->person: weight float"),
        "{read}"
    );
}

#[test]
fn the_default_listing_stays_within_its_budget() {
    // Characters of the instructions and every tools/list entry without a configuration.
    // A failure here means a description grew: shorten it, or raise the budget deliberately
    // (it is what every agent pays in context before its first call).
    let host = TestHost::modern();
    for (read_only, instructions, tools, tool_count) in
        [(false, 3_471, 24_000, 27), (true, 3_577, 15_500, 19)]
    {
        let cost = Mcp::new(read_only, None, false)
            .prompt_cost(&host)
            .expect("measures");
        assert_eq!(
            cost.instructions_chars, instructions,
            "read_only {read_only}"
        );
        assert_eq!(cost.tools.len(), tool_count);
        assert!(
            cost.tools_chars() <= tools,
            "read_only {read_only}: tools take {} chars, budget {tools}",
            cost.tools_chars()
        );
    }
}

#[test]
fn the_knowledge_graph_example_stays_within_its_budget() {
    let host = TestHost::with_queries();
    let config = parse(KB_EXAMPLE).expect("the example parses");
    config
        .check(true)
        .expect("the example fits a read-only server");
    let mcp = Mcp::new(true, None, false).with_config(config);
    let cost = mcp.prompt_cost(&host).expect("measures");
    let listed: Vec<&str> = cost.tools.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        listed,
        ["gql", "run_saved_query", "get_schema", "deep", "older"]
    );
    // The built-in part (instructions, gql, run_saved_query, get_schema with its summary option
    // `descriptions`): about 900 tokens; each saved-query tool adds its own description and
    // parameters.
    let builtin: usize =
        cost.instructions_chars + cost.tools.iter().take(3).map(|(_, c)| c).sum::<usize>();
    assert!(
        builtin <= 3_750,
        "the example's built-in part takes {builtin} chars"
    );
    assert!(PromptCost::tokens(cost.total_chars()) <= 1_200, "{cost:?}");
}
