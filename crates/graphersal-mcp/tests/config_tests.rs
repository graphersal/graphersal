//! The operator's configuration (`McpConfig`, `--mcp-config`): parsing and its errors, what a
//! configured endpoint lists and refuses, short descriptions, and the prompt budgets (a long
//! new description shows here).

use std::sync::Mutex;
use std::sync::mpsc::{Sender, channel};

use graphersal_mcp::{
    ClientRef, Descriptions, Executor, Host, Job, Mcp, McpConfig, McpError, McpRequest, PromptCost,
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
    assert!(err.contains("\"full\" or \"short\""), "{err}");
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
    // Full descriptions: the folder tail and the paging parameters stay.
    assert!(
        older["description"]
            .as_str()
            .unwrap()
            .contains("Folder: kb."),
        "{older}"
    );
    assert!(older["inputSchema"]["properties"].get("max_rows").is_some());

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
fn short_descriptions_are_short_and_drop_the_saved_query_boilerplate() {
    let host = TestHost::with_queries();
    let full = Mcp::new(false, None, true)
        .tool_listings(&host)
        .expect("lists");
    let short = Mcp::new(false, None, false)
        .with_config(
            parse(r#"{"descriptions": "short", "saved_query_tools": {}}"#).expect("parses"),
        )
        .tool_listings(&host)
        .expect("lists");
    assert_eq!(names(&full), names(&short));
    for (full, short) in full.iter().zip(&short).take(27) {
        let (f, s) = (
            full["description"].as_str().unwrap(),
            short["description"].as_str().unwrap(),
        );
        assert!(!s.is_empty() && s.len() < 520, "{}: {s}", full["name"]);
        assert!(s.len() <= f.len(), "{}", full["name"]);
        // The input schemas never change.
        assert_eq!(
            full["inputSchema"], short["inputSchema"],
            "{}",
            full["name"]
        );
    }
    let gql = short.iter().find(|t| t["name"] == "gql").unwrap();
    assert!(
        gql["description"]
            .as_str()
            .unwrap()
            .contains("graphersal://gql-reference")
    );
    let older = short.iter().find(|t| t["name"] == "older").unwrap();
    assert_eq!(older["description"], "People older than min.");
    assert_eq!(
        older["inputSchema"]["properties"],
        json!({"min": {"type": "integer", "default": 30}})
    );
    // paging_params: true keeps the paging in short mode; false drops it in full mode.
    for (config, paging) in [
        (
            r#"{"descriptions": "short", "saved_query_tools": {"paging_params": true}}"#,
            true,
        ),
        (r#"{"saved_query_tools": {"paging_params": false}}"#, false),
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
    let mcp = Mcp::new(false, None, false)
        .with_config(parse(r#"{"descriptions": "short", "saved_query_tools": {}}"#).unwrap());
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
fn the_gql_reference_is_readable_and_listed_only_when_configured() {
    let host = TestHost::modern();
    let mcp = Mcp::new(false, None, false);
    let listed = rpc(&mcp, &host, "resources/list", json!({}));
    assert_eq!(listed["result"]["resources"].as_array().unwrap().len(), 5);
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
fn the_default_listing_stays_within_its_budget() {
    // Characters of the instructions and every tools/list entry without a configuration.
    // A failure here means a description grew: shorten it, or raise the budget deliberately
    // (it is what every agent pays in context before its first call).
    let host = TestHost::modern();
    for (read_only, instructions, tools, tool_count) in
        [(false, 3_471, 47_000, 27), (true, 3_577, 37_000, 19)]
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
    assert_eq!(config.descriptions(), Descriptions::Short);
    let mcp = Mcp::new(true, None, false).with_config(config);
    let cost = mcp.prompt_cost(&host).expect("measures");
    let listed: Vec<&str> = cost.tools.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        listed,
        ["gql", "run_saved_query", "get_schema", "deep", "older"]
    );
    // The built-in part (instructions, gql, run_saved_query, get_schema): about 900 tokens;
    // each saved-query tool adds its own description and parameters.
    let builtin: usize =
        cost.instructions_chars + cost.tools.iter().take(3).map(|(_, c)| c).sum::<usize>();
    assert!(
        builtin <= 3_700,
        "the example's built-in part takes {builtin} chars"
    );
    assert!(PromptCost::tokens(cost.total_chars()) <= 1_200, "{cost:?}");
}
