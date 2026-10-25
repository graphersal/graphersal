//! `graphersal`: the `graphersal>` REPL and a one-shot, scriptable Gremlin query runner.

// Output goes through `out.rs`: the print macros panic on a closed pipe.
#![deny(clippy::print_stdout, clippy::print_stderr)]

#[macro_use]
mod out;
#[cfg(feature = "ai")]
mod ai;
mod args_file;
mod cli;
#[cfg(feature = "model2vec")]
mod embed_model;
mod log;
mod repl;
#[cfg(feature = "dev-server")]
mod server;
mod signals;
mod store_cmd;

// `graphersal` builds with `graphersal`'s `alloc-tracking` feature on by default (see this crate's
// Cargo.toml) and installs the tracking allocator here, as its own binary — a library crate
// cannot set `#[global_allocator]` for its consumer. This makes every `graphersal` run (REPL and
// one-shot) report `MemorySource::Tracked` (exact, not estimated) figures in `.profile()`'s `Mem`
// column, at the small always-on per-allocation bookkeeping cost `alloc-tracking`'s doc comment
// describes (see CLAUDE.md and the memory-inspection spec's D8 benchmark for the measured tax).
// `graphersal`'s synchronous, single-threaded-per-command execution model means a query's thread-local
// allocation counter is never split across threads mid-query.
#[global_allocator]
static ALLOC: graphersal::alloc_tracking::TrackingAllocator =
    graphersal::alloc_tracking::TrackingAllocator::new();

use std::io::IsTerminal;
use std::process::exit;

use crate::cli::{Cli, GraphSpec, Mode};

use rustyline::Result;

/// Stack size of the thread scripts run on: the scripting engine copies nested values
/// recursively (a few thousand nested maps overflowed the 8 MiB main thread of a debug build
/// before any Graphersal code ran), so the whole session runs on a thread with a larger stack.
/// Virtual memory only: pages are committed as the stack is actually used.
const SCRIPT_STACK_BYTES: usize = 64 * 1024 * 1024;

fn main() -> Result<()> {
    let session = std::thread::Builder::new()
        .name("graphersal".to_string())
        .stack_size(SCRIPT_STACK_BYTES)
        .spawn(run);
    match session {
        Ok(handle) => match handle.join() {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        },
        // No thread could be started: run on the main thread with its default stack.
        Err(_) => run(),
    }
}

/// The whole command line session (one-shot or REPL), on the large-stack thread of [`main`].
fn run() -> Result<()> {
    // Scripts of the command line count under `cli` in a store's workload statistics (the dev
    // server sets its own channels per request).
    graphersal::workload::set_thread_default(graphersal::workload::Channel::Cli, false);
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("store") {
        log::install();
        exit(out::exit_code(store_cmd::run(&args[1..])));
    }
    if args.first().map(String::as_str) == Some("embed-model") {
        #[cfg(feature = "model2vec")]
        exit(out::exit_code(embed_model::run(&args[1..])));
        #[cfg(not(feature = "model2vec"))]
        {
            errln!(
                "embed-model is not available: this graphersal was built without the \
                 `model2vec` feature"
            );
            exit(cli::EXIT_USAGE_ERROR);
        }
    }
    let options = match Cli::parse(args) {
        Ok(options) => options,
        Err(err) => {
            errln!("{err}\n\nRun `graphersal --help` for usage.");
            exit(cli::EXIT_USAGE_ERROR);
        }
    };
    // The embedding models of --embedder are loaded once, before any graph or script: every
    // mode (one-shot, REPL, --server and its MCP tools) calls the same registry.
    if !matches!(options.mode, Mode::Help | Mode::Version)
        && let Err(err) = cli::install_embedders(&options)
    {
        errln!("{err}");
        exit(cli::EXIT_USAGE_ERROR);
    }

    // Every mode that opens a Store (`-e`, `--in`, a script file, the REPL) prints the
    // library's store warnings (a failed automatic checkpoint, damage found, a read-only open)
    // on stderr; stdout stays the result only.
    if matches!(options.graph, GraphSpec::Store(_))
        && !matches!(options.mode, Mode::Help | Mode::Version)
    {
        log::install();
    }
    match options.mode {
        Mode::Help => {
            out!("{}", cli::HELP);
            exit(out::exit_code(cli::EXIT_OK));
        }
        Mode::Version => {
            outln!("graphersal {}", env!("CARGO_PKG_VERSION"));
            exit(out::exit_code(cli::EXIT_OK));
        }
        // A piped or redirected stdin is a one-shot invocation even without `--in`, so that
        // `echo '...' | graphersal` does not block on an interactive prompt.
        Mode::Repl if !options.repl && !std::io::stdin().is_terminal() => {
            exit(out::exit_code(cli::run_eval(&Cli {
                read_stdin: true,
                ..options
            })));
        }
        Mode::Eval => exit(out::exit_code(cli::run_eval(&options))),
        #[cfg(feature = "dev-server")]
        Mode::Server => {
            // Store warnings (a failed automatic checkpoint, damage found, a read-only open)
            // are `tracing` events of the library; the server prints them on stderr.
            log::install();
            exit(server::run(&options))
        }
        #[cfg(not(feature = "dev-server"))]
        Mode::Server => {
            errln!(
                "--server is not available: this graphersal was built without the `dev-server` \
                 feature"
            );
            exit(cli::EXIT_USAGE_ERROR);
        }
        Mode::Repl => {}
    }

    let result = repl::run(options);
    // The REPL ends when its stdout went away; say so with the exit code.
    if !out::stdout_open() {
        exit(out::exit_code(cli::EXIT_OK));
    }
    result
}
