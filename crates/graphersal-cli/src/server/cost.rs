//! `graphersal mcp-cost`: what the MCP endpoint's prompt (the `initialize` instructions and the
//! `tools/list` entries) costs an agent's context, for a graph and a configuration, without
//! starting a server. The graph is always opened read-only (nothing is written, a Store's lock
//! is not taken), so it can measure a store a running server holds.

use std::sync::Mutex;
use std::sync::mpsc::{Sender, channel};

use graphersal_mcp::{ClientRef, Executor, Host, McpError, PromptCost};
use serde_json::Value;

use super::{Job, graph_title, session_presets, start_session};
use crate::cli::{Cli, EXIT_OK, EXIT_USAGE_ERROR};
use crate::out::{Note, print_block, usage_text};

/// `graphersal mcp-cost --help`.
const HELP: &str = "graphersal mcp-cost - what the MCP prompt costs an agent's context

USAGE:
    graphersal mcp-cost [--graph SRC] [--mcp-config FILE] [--read-only] [--mcp-query-tools]
                        [--mcp-instructions FILE]

Prints the characters of the instructions an MCP client gets from initialize and of every
tool entry of tools/list, with a rough token estimate (characters / 4; a real tokenizer
counts differently), as `graphersal --graph SRC --server --mcp ...` would serve them. No
server is started; the graph is opened read-only (nothing is written, a Store's lock is not
taken), so a store a running server holds can be measured.

OPTIONS:
    -g, --graph <SRC>        The graph (default modern): its saved queries become tools with
                             --mcp-query-tools or the configuration's saved_query_tools.
        --mcp-config <FILE>  The operator's MCP configuration (graphersal --help).
        --read-only          Measure a read-only endpoint (--mcp-read-only, --read-only):
                             only the reading tools. --mcp-read-only is the same.
        --mcp-query-tools    Every saved query a tool of its own.
        --mcp-instructions <FILE>
                             The operator's notes appended to the instructions.
    -h, --help               Print this help.";

/// Runs `graphersal mcp-cost <args>`; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        outln!("{HELP}");
        return EXIT_OK;
    }
    // The server's own parser checks the flags and reads the configuration.
    let mut server_args: Vec<String> = vec!["--server".into(), "--mcp".into()];
    let mut read_only = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "--read-only" | "--mcp-read-only" => read_only = true,
            "--mcp-query-tools" => server_args.push(flag.to_string()),
            "-g" | "--graph" | "--mcp-config" | "--mcp-instructions" => {
                let Some(value) = inline.or_else(|| iter.next().cloned()) else {
                    errln!(
                        "{}",
                        usage_text(
                            &format!("Option {flag} requires a value"),
                            "graphersal mcp-cost --help"
                        )
                    );
                    return EXIT_USAGE_ERROR;
                };
                server_args.push(flag.to_string());
                server_args.push(value);
            }
            other => {
                errln!(
                    "{}",
                    usage_text(
                        &format!("Unknown mcp-cost option: {other}"),
                        "graphersal mcp-cost --help"
                    )
                );
                return EXIT_USAGE_ERROR;
            }
        }
    }
    if read_only {
        server_args.push("--mcp-read-only".into());
    }
    let cli = match Cli::parse(server_args) {
        Ok(cli) => cli,
        Err(err) => {
            errln!("{}", usage_text(&err, "graphersal mcp-cost --help"));
            return EXIT_USAGE_ERROR;
        }
    };
    crate::cli::defer_model_notes();
    let opened = match cli.graph.open(false, None, None, true) {
        Ok(opened) => opened,
        Err(err) => {
            errln!("{}", err.message);
            return err.code;
        }
    };
    let title = graph_title(&cli.graph);
    let started = start_session(
        &cli.graph,
        None,
        session_presets(&cli),
        None,
        Some((opened.graph, title.clone())),
        None,
    );
    let jobs = match started {
        Ok((jobs, ..)) => jobs,
        Err(message) => {
            errln!("{message}");
            return EXIT_USAGE_ERROR;
        }
    };
    let mcp = match &cli.mcp_config {
        Some(config) => {
            graphersal_mcp::Mcp::new(read_only, None, false).with_config(config.clone())
        }
        None => graphersal_mcp::Mcp::new(read_only, None, cli.mcp_query_tools)
            .with_operator_notes(cli.mcp_instructions.clone()),
    };
    let host = CostHost {
        jobs: Mutex::new(jobs),
    };
    let cost = match mcp.prompt_cost(&host) {
        Ok(cost) => cost,
        Err(err) => {
            errln!("Error: cannot list the tools: {err}");
            return 1;
        }
    };
    print_block(&summary(&cost, &title, read_only, cli.mcp_config.is_some()));
    print_block(&tool_rows(&cost));
    EXIT_OK
}

/// The headline block: the totals.
fn summary(cost: &PromptCost, graph: &str, read_only: bool, configured: bool) -> String {
    let tokens = |chars: usize| format!("{chars} chars, ~{} tokens", PromptCost::tokens(chars));
    Note::new(format!(
        "MCP prompt cost of {graph} ({}, {})",
        if read_only { "read-only" } else { "read-write" },
        if configured {
            "with --mcp-config"
        } else {
            "built-in configuration"
        }
    ))
    .detail("instructions", tokens(cost.instructions_chars))
    .detail(
        "tools",
        format!("{}: {}", cost.tools.len(), tokens(cost.tools_chars())),
    )
    .detail("total", tokens(cost.total_chars()))
    .detail(
        "estimate",
        "tokens = characters / 4, a rough estimate (a real tokenizer counts differently)",
    )
    .text()
}

/// One row per tool, largest first, the names padded to one column.
fn tool_rows(cost: &PromptCost) -> String {
    let mut tools = cost.tools.clone();
    tools.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let width = tools.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
    let mut text = String::from("Tools (characters of each tools/list entry, largest first)");
    for (name, chars) in tools {
        text.push_str(&format!(
            "\n  {:<width$}  {chars:>6} chars  ~{} tokens",
            format!("{name}:"),
            PromptCost::tokens(chars),
            width = width + 1
        ));
    }
    text
}

/// The session thread of the measured graph; the host has no server state.
struct CostHost {
    jobs: Mutex<Sender<Job>>,
}

impl Executor for CostHost {
    fn execute(&self, _client: &ClientRef, job: Job) -> Result<(), McpError> {
        let (done, wait) = channel();
        let sent = self.jobs.lock().is_ok_and(|jobs| {
            jobs.send(Box::new(move |session| {
                job(session);
                let _ = done.send(());
            }))
            .is_ok()
        });
        if !sent {
            return Err(McpError::Unavailable("the session is not running".into()));
        }
        wait.recv()
            .map_err(|_| McpError::Unavailable("the job failed inside the session".into()))
    }
}

impl Host for CostHost {
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
