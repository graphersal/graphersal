//! The interactive `graphersal>` shell: slash commands (`/help`, `/set ...`, `/exit`), Tab
//! completion and `/help <name>`.
//!
//! Completion and help read ONE source, the same as the playground's query editor: the
//! completion catalogue of `graphersal_session::views` (the Rhai engine's own function metadata
//! with the doc texts of the engine's doc table, and the token values from the getters the
//! engine registers). A step documented there shows up here with no change to this file.

use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::process::exit;

use graphersal::exec::{RenderOptions, Spelling};
use graphersal::schema::VisualizeFormat;
use graphersal::script::ScriptLimits;
use graphersal_session::views::{CompletionEntry, completion_entries, token_values};
use rhai::{Dynamic, Engine, Scope};
use rustyline::completion::{Completer, Pair};
use rustyline::config::Builder;
use rustyline::error::ReadlineError;
use rustyline::hint::{Hint, Hinter};
use rustyline::validate::{ValidationContext, ValidationResult, Validator};
use rustyline::{Context, Editor, Result};
use rustyline_derive::{Helper, Highlighter};

use crate::cli::{self, Cli};
use crate::signals;

/// One option of `/set`: its name, its argument and what it does.
struct SetOption {
    name: &'static str,
    args: &'static str,
    group: &'static str,
    summary: &'static str,
}

/// The `/set` options, in the order `/help` lists them.
const SET_OPTIONS: &[SetOption] = &[
    SetOption {
        name: "format",
        args: "<fmt>",
        group: "Display",
        summary: "default visualizer of results without a data terminal",
    },
    SetOption {
        name: "max-rows",
        args: "<n>",
        group: "Display",
        summary: "show at most n top-level results (default 100)",
    },
    SetOption {
        name: "max-items",
        args: "<n>",
        group: "Display",
        summary: "show at most n items of every nested list or map (default 100)",
    },
    SetOption {
        name: "no-limit",
        args: "",
        group: "Display",
        summary: "show results completely",
    },
    SetOption {
        name: "spelling",
        args: "<snake|camel>",
        group: "Display",
        summary: "step names in completion, help, .profile() and errors",
    },
    SetOption {
        name: "max-operations",
        args: "<n>",
        group: "Script limits",
        summary: "Rhai operations one input may run (0 = unlimited)",
    },
    SetOption {
        name: "max-string-size",
        args: "<n>",
        group: "Script limits",
        summary: "longest string a script or a step may build, in bytes",
    },
    SetOption {
        name: "max-array-size",
        args: "<n>",
        group: "Script limits",
        summary: "largest array a script may build",
    },
    SetOption {
        name: "max-map-size",
        args: "<n>",
        group: "Script limits",
        summary: "largest map a script may build",
    },
    SetOption {
        name: "max-value-depth",
        args: "<n>",
        group: "Script limits",
        summary: "deepest nesting of one value (0 = the default, 128)",
    },
    SetOption {
        name: "safe-limits",
        args: "",
        group: "Script limits",
        summary: "the SAFE resource limits of the library",
    },
];

/// The commands: name, argument, summary.
const COMMANDS: &[(&str, &str, &str)] = &[
    (
        "help",
        "[name]",
        "this list; with a name: the doc of a step, token or command",
    ),
    (
        "set",
        "<option> [value]",
        "change a session setting (/help set lists them)",
    ),
    (
        "memory",
        "[max_elements]",
        "where the graph's memory goes per label and property (g.memory_report()); \
         with a number: sampled",
    ),
    ("exit", "", "close the graph and leave the shell"),
];

/// The text of `/memory [max_elements]`: the memory report of `graph`, sampled when a number is
/// given.
fn memory_report(graph: &graphersal::storage::Graph, args: &str) -> String {
    let mut request = graphersal::storage::MemoryReportRequest::new();
    let args = args.trim();
    if !args.is_empty() {
        match args.parse::<u64>() {
            Ok(n) if n > 0 => request = request.with_max_elements(n),
            _ => return "Usage: /memory [max_elements] (a positive number samples)".to_string(),
        }
    }
    match graphersal::storage::AnyGraph::memory_report(graph, &request) {
        Ok(report) => report.to_string(),
        Err(err) => graphersal::error::format_diagnostic(&*err),
    }
}

/// Rhai keywords offered at the start of an expression.
const KEYWORDS: &[&str] = &[
    "let", "const", "fn", "if", "else", "for", "in", "while", "loop", "return", "true", "false",
];

/// What a line of input is.
#[derive(Debug, PartialEq, Eq)]
pub enum Input<'a> {
    /// A slash command: its name and the rest of the line (trimmed).
    Command(&'a str, &'a str),
    /// A line in the removed `:` form: the one-line pointer to its `/` form.
    OldPrefix(String),
    /// A script (statements), evaluated by the engine.
    Script,
}

/// Classifies a line: a `/` command (but not a `//` comment), the removed `:` prefix, or a script.
pub fn classify(line: &str) -> Input<'_> {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix('/')
        && !rest.starts_with('/')
    {
        let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        return Input::Command(name, args.trim());
    }
    if let Some(rest) = trimmed.strip_prefix(':') {
        return Input::OldPrefix(format!(
            "Commands start with `/`: use `/{}` (/help lists them)",
            rest.trim()
        ));
    }
    Input::Script
}

/// The completion and help catalogue of one session: the engine's DSL functions, the token
/// classes of the scope with their values, and the scope's global names.
pub struct Catalogue {
    entries: Vec<CompletionEntry>,
    /// Token class -> values (`Order` -> `asc`, `desc`, `shuffle`), only classes in the scope.
    tokens: BTreeMap<String, Vec<String>>,
    /// The scope's names (`g`, `__`, `P`, `Order`, `keys`, ...).
    globals: Vec<String>,
}

impl Catalogue {
    /// Builds the catalogue from the session's engine and scope.
    pub fn new(engine: &Engine, scope: &Scope<'_>) -> Self {
        let globals: Vec<String> = scope.iter().map(|(name, _, _)| name.to_string()).collect();
        let mut tokens = token_values(engine);
        tokens.retain(|class, _| globals.iter().any(|g| g == class));
        Self {
            entries: completion_entries(engine),
            tokens,
            globals,
        }
    }

    /// The name of `entry` in `spelling`.
    fn spelled(entry: &CompletionEntry, spelling: Spelling) -> &str {
        match spelling {
            Spelling::Camel => entry.camel_name(),
            _ => &entry.name,
        }
    }

    /// Whether `name` is a receiver of functions or a token class (`P`, `TextP`, `Order`, ...).
    fn is_class(&self, name: &str) -> bool {
        self.tokens.contains_key(name)
            || (name != "traversal"
                && name != "global"
                && self.entries.iter().any(|e| e.receiver == name))
    }

    /// The completion candidates at `pos` of `line`: the start of the replaced word and the
    /// candidates.
    pub fn complete(&self, line: &str, pos: usize, spelling: Spelling) -> (usize, Vec<Pair>) {
        let pos = pos.min(line.len());
        let before = &line[..pos];
        let lead = before.len() - before.trim_start().len();
        if matches!(classify(before), Input::Command(..)) {
            return self.complete_command(before, lead, spelling);
        }
        let start = before
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_')
            .last()
            .map_or(pos, |(i, _)| i);
        let partial = &before[start..];
        let prefix = &before[..start];
        // Inside a string literal: nothing to complete.
        if prefix.chars().filter(|c| *c == '"').count() % 2 == 1 {
            return (pos, Vec::new());
        }
        let access = prefix.trim_end();
        let candidates = if let Some(head) = access
            .strip_suffix("::")
            .or_else(|| access.strip_suffix('.'))
        {
            let head = head.trim_end();
            let ident_start = head
                .char_indices()
                .rev()
                .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_')
                .last()
                .map_or(head.len(), |(i, _)| i);
            let ident = &head[ident_start..];
            if self.is_class(ident) {
                let mut out = self.function_candidates(ident, partial, spelling);
                for value in self.tokens.get(ident).into_iter().flatten() {
                    if value.starts_with(partial) {
                        out.push(plain(value));
                    }
                }
                out
            } else {
                self.function_candidates("traversal", partial, spelling)
            }
        } else {
            let mut out: Vec<Pair> = self
                .globals
                .iter()
                .map(String::as_str)
                .chain(KEYWORDS.iter().copied())
                .filter(|n| n.starts_with(partial))
                .map(plain)
                .collect();
            out.extend(self.function_candidates("global", partial, spelling));
            out
        };
        (start, sorted(candidates))
    }

    /// The functions of `receiver` starting with `partial`, in `spelling`; when none matches,
    /// every spelling is tried (`hasL` in a snake_case session still completes).
    fn function_candidates(&self, receiver: &str, partial: &str, spelling: Spelling) -> Vec<Pair> {
        let of_receiver = || self.entries.iter().filter(|e| e.receiver == receiver);
        let mut out: Vec<Pair> = of_receiver()
            .filter(|e| Self::spelled(e, spelling).starts_with(partial))
            .map(|e| call(Self::spelled(e, spelling), e))
            .collect();
        if out.is_empty() && !partial.is_empty() {
            out = of_receiver()
                .flat_map(|e| e.spellings().map(move |s| (s, e)))
                .filter(|(s, _)| s.starts_with(partial))
                .map(|(s, e)| call(s, e))
                .collect();
        }
        out
    }

    /// Completion of a `/` command line.
    fn complete_command(
        &self,
        before: &str,
        lead: usize,
        spelling: Spelling,
    ) -> (usize, Vec<Pair>) {
        let text = &before[lead..];
        let start = text.rfind(char::is_whitespace).map_or(0, |i| i + 1);
        let partial = &text[start..];
        let words: Vec<&str> = text[..start].split_whitespace().collect();
        let names: Vec<String> = match words.as_slice() {
            [] => COMMANDS
                .iter()
                .map(|(n, args, _)| {
                    if args.is_empty() {
                        format!("/{n}")
                    } else {
                        format!("/{n} ")
                    }
                })
                .collect(),
            ["/set"] => SET_OPTIONS
                .iter()
                .map(|o| {
                    if o.args.is_empty() {
                        o.name.to_string()
                    } else {
                        format!("{} ", o.name)
                    }
                })
                .collect(),
            ["/set", "format"] => VisualizeFormat::names()
                .iter()
                .map(|n| n.to_string())
                .collect(),
            ["/set", "spelling"] => vec!["snake".into(), "camel".into()],
            ["/help"] => self.help_topics(spelling),
            _ => Vec::new(),
        };
        let candidates = names
            .into_iter()
            .filter(|n| n.starts_with(partial))
            .map(|n| Pair {
                display: n.trim_end().to_string(),
                replacement: n,
            })
            .collect();
        (lead + start, sorted(candidates))
    }

    /// Every name `/help` knows: topics, commands, token classes and functions (in `spelling`).
    fn help_topics(&self, spelling: Spelling) -> Vec<String> {
        let mut names: BTreeSet<String> = ["steps", "tokens"].map(String::from).into();
        names.extend(COMMANDS.iter().map(|(n, _, _)| n.to_string()));
        names.extend(self.tokens.keys().cloned());
        for entry in &self.entries {
            let name = Self::spelled(entry, spelling);
            match entry.receiver.as_str() {
                "traversal" | "global" => names.insert(name.to_string()),
                receiver => names.insert(format!("{receiver}.{name}")),
            };
        }
        names.into_iter().collect()
    }

    /// The `/help` text for `topic` (empty: the command list).
    pub fn help(&self, topic: &str, spelling: Spelling) -> String {
        let topic = topic.trim().trim_start_matches('/');
        match topic {
            "" => command_list(),
            "steps" => self.step_overview(spelling),
            "tokens" => self.token_overview(spelling),
            "set" => set_help(),
            _ => {
                if let Some((_, args, summary)) = COMMANDS.iter().find(|(n, _, _)| *n == topic) {
                    return format!("/{topic} {args}\n  {summary}").replace("  \n", "\n");
                }
                if let Some(option) = topic
                    .strip_prefix("set ")
                    .and_then(|o| SET_OPTIONS.iter().find(|s| s.name == o.trim()))
                {
                    return format!("/set {} {}\n  {}", option.name, option.args, option.summary);
                }
                self.name_help(topic, spelling)
            }
        }
    }

    /// The help of a function or token name (`has_label`, `hasLabel`, `P.eq`, `Order`).
    fn name_help(&self, topic: &str, spelling: Spelling) -> String {
        let (receiver, name) = match topic.split_once(['.', ':']) {
            Some((class, name)) if self.is_class(class) => {
                (Some(class), name.trim_start_matches(':'))
            }
            _ => (None, topic),
        };
        if receiver.is_none() && self.is_class(name) {
            return self.class_help(name, spelling);
        }
        let key = |s: &str| s.replace('_', "").to_ascii_lowercase();
        let matches = |exact: bool| -> Vec<&CompletionEntry> {
            let mut found: Vec<&CompletionEntry> = self
                .entries
                .iter()
                .filter(|e| receiver.is_none_or(|r| e.receiver == r))
                .filter(|e| {
                    e.spellings().any(|s| {
                        if exact {
                            s == name
                        } else {
                            key(s) == key(name)
                        }
                    })
                })
                .collect();
            // Steps first, then free functions, then the token classes' functions.
            found.sort_by_key(|e| match e.receiver.as_str() {
                "traversal" => 0,
                "global" => 1,
                _ => 2,
            });
            found
        };
        let mut found = matches(true);
        if found.is_empty() {
            found = matches(false);
        }
        if found.is_empty() {
            if let Some(class) = receiver
                && self
                    .tokens
                    .get(class)
                    .is_some_and(|v| v.iter().any(|m| m == name))
            {
                return format!(
                    "{class}.{name}: a value of the token class {class} (/help {class})"
                );
            }
            return self.unknown_help(topic, spelling);
        }
        found
            .iter()
            .map(|e| entry_help(e, spelling))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// A token class: its values and its functions.
    fn class_help(&self, class: &str, spelling: Spelling) -> String {
        let mut out = format!("{class}: token class");
        if let Some(values) = self.tokens.get(class) {
            out.push_str(&format!(
                "\n  values: {}",
                values
                    .iter()
                    .map(|v| format!("{class}.{v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let functions: Vec<String> = self
            .entries
            .iter()
            .filter(|e| e.receiver == class)
            .map(|e| format!("{class}.{}", Self::spelled(e, spelling)))
            .collect();
        if !functions.is_empty() {
            out.push_str("\n  functions:\n");
            out.push_str(&grid(&functions, 4));
            out.push_str(&format!("  /help {class}.<name> describes one."));
        }
        out
    }

    /// An unknown name: the pointer to the lists, with the names it is a prefix of.
    fn unknown_help(&self, topic: &str, spelling: Spelling) -> String {
        let close: Vec<String> = self
            .help_topics(spelling)
            .into_iter()
            .filter(|n| !topic.is_empty() && n.starts_with(topic))
            .take(8)
            .collect();
        let mut out = format!(
            "No help for `{topic}`: not a command, step, token or function. /help steps lists \
             the steps, /help tokens the token classes."
        );
        if !close.is_empty() {
            out.push_str(&format!("\nDid you mean: {}", close.join(", ")));
        }
        out
    }

    /// `/help steps`: every function of a traversal, in `spelling`.
    fn step_overview(&self, spelling: Spelling) -> String {
        let names: BTreeSet<String> = self
            .entries
            .iter()
            .filter(|e| e.receiver == "traversal")
            .map(|e| Self::spelled(e, spelling).to_string())
            .collect();
        let names: Vec<String> = names.into_iter().collect();
        format!(
            "Steps, terminals and source methods ({} spelling; /set spelling switches):\n{}\
             /help <name> shows one (either spelling).",
            spelling.as_str(),
            grid(&names, 5)
        )
    }

    /// `/help tokens`: every token class with its values.
    fn token_overview(&self, spelling: Spelling) -> String {
        let mut classes: BTreeSet<&str> = self.tokens.keys().map(String::as_str).collect();
        classes.extend(
            self.entries
                .iter()
                .map(|e| e.receiver.as_str())
                .filter(|r| self.globals.iter().any(|g| g == r)),
        );
        let mut out =
            String::from("Token classes (/help <class> lists its values and functions):\n");
        for class in classes {
            let mut members: Vec<String> = self.tokens.get(class).cloned().unwrap_or_default();
            members.extend(
                self.entries
                    .iter()
                    .filter(|e| e.receiver == class)
                    .map(|e| format!("{}()", Self::spelled(e, spelling))),
            );
            let mut line = members.join(", ");
            if line.chars().count() > 80 {
                line = line.chars().take(77).collect::<String>() + "...";
            }
            out.push_str(&format!("  {class:<12} {line}\n"));
        }
        out.trim_end().to_string()
    }
}

/// A candidate inserted as is.
fn plain(name: &str) -> Pair {
    Pair {
        display: name.to_string(),
        replacement: name.to_string(),
    }
}

/// A function candidate: `name(` (`name()` when it takes no argument).
fn call(name: &str, entry: &CompletionEntry) -> Pair {
    let replacement = if entry.arities == [0] {
        format!("{name}()")
    } else {
        format!("{name}(")
    };
    Pair {
        display: name.to_string(),
        replacement,
    }
}

/// Candidates sorted by display, duplicates dropped.
fn sorted(mut candidates: Vec<Pair>) -> Vec<Pair> {
    candidates.sort_by(|a, b| a.display.cmp(&b.display));
    candidates.dedup_by(|a, b| a.display == b.display);
    candidates
}

/// The help of one catalogue entry, like the playground's tooltip: the name with its other
/// spellings, the overloads, the doc sentence and the example with its result.
fn entry_help(entry: &CompletionEntry, spelling: Spelling) -> String {
    let name = Catalogue::spelled(entry, spelling);
    let qualified = match entry.receiver.as_str() {
        "traversal" | "global" => name.to_string(),
        receiver => format!("{receiver}.{name}"),
    };
    let kind = match entry.receiver.as_str() {
        "traversal" => "step, terminal or source method of a traversal".to_string(),
        "global" => "function".to_string(),
        receiver => format!("function of {receiver}"),
    };
    let others: Vec<&str> = entry.spellings().filter(|s| *s != name).collect();
    let mut out = format!("{qualified}  ({kind})");
    if !others.is_empty() {
        out.push_str(&format!("\n  also: {}", others.join(", ")));
    }
    for signature in &entry.signatures {
        out.push_str(&format!("\n  {qualified}{signature}"));
    }
    let doc = if entry.doc.is_empty() {
        "See the signatures above; the Graphersal book describes it."
    } else {
        entry.doc.as_str()
    };
    out.push_str("\n\n");
    out.push_str(doc);
    out
}

/// `/help`: the commands, grouped.
fn command_list() -> String {
    let mut out = String::from(
        "Commands (Tab completes them, their arguments, steps and tokens):\n\nHelp\n  \
         /help                        this list\n  \
         /help <name>                 a step, function, token class or command, in either \
         spelling\n                               (/help has_label, /help hasLabel, /help P.eq, \
         /help Order)\n  \
         /help steps                  every step and terminal\n  \
         /help tokens                 the token classes and their values\n",
    );
    let mut group = "";
    for option in SET_OPTIONS {
        if option.group != group {
            group = option.group;
            out.push_str(&format!("\n{group}\n"));
        }
        let usage = format!("/set {} {}", option.name, option.args);
        out.push_str(&format!("  {:<28} {}\n", usage.trim_end(), option.summary));
    }
    out.push_str(
        "\nSession\n  /exit                        close the graph and leave (Ctrl-D too)\n\n\
         A script runs when its input ends with `;`; Enter on a line without it continues the \
         input.",
    );
    out
}

/// `/help set`: the `/set` options.
fn set_help() -> String {
    let mut out = String::from("/set <option> [value]:\n");
    for option in SET_OPTIONS {
        let usage = format!("/set {} {}", option.name, option.args);
        out.push_str(&format!("  {:<28} {}\n", usage.trim_end(), option.summary));
    }
    out.trim_end().to_string()
}

/// `items` as rows of `columns` cells, padded to the widest item.
fn grid(items: &[String], columns: usize) -> String {
    let width = items.iter().map(|i| i.chars().count()).max().unwrap_or(0);
    let mut out = String::new();
    for row in items.chunks(columns.max(1)) {
        let line = row
            .iter()
            .map(|item| format!("{item:<width$}"))
            .collect::<Vec<_>>()
            .join("  ");
        out.push_str("  ");
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// The rustyline helper of the shell: completion, command hints and the `;` validator.
#[derive(Helper, Highlighter)]
pub struct ReplHelper {
    catalogue: Catalogue,
    spelling: Spelling,
}

impl Completer for ReplHelper {
    type Candidate = Pair;

    fn complete(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Result<(usize, Vec<Pair>)> {
        Ok(self.catalogue.complete(line, pos, self.spelling))
    }
}

impl Validator for ReplHelper {
    fn validate(&self, ctx: &mut ValidationContext) -> Result<ValidationResult> {
        let input = ctx.input();
        if input.trim().is_empty() || !matches!(classify(input), Input::Script) {
            return Ok(ValidationResult::Valid(None));
        }
        Ok(if input.trim().ends_with(';') {
            ValidationResult::Valid(None)
        } else {
            ValidationResult::Incomplete
        })
    }
}

/// A greyed hint after a partly typed command: the rest of its usage. Accepting it (Right
/// arrow) inserts only the literal part, never the `<n>` placeholder.
pub struct CommandHint {
    display: String,
    complete_up_to: usize,
}

impl Hint for CommandHint {
    fn display(&self) -> &str {
        &self.display
    }

    fn completion(&self) -> Option<&str> {
        (self.complete_up_to > 0).then(|| &self.display[..self.complete_up_to])
    }
}

/// The usage of every command, for [`command_hint`].
fn command_usages() -> Vec<String> {
    let mut usages: Vec<String> = COMMANDS
        .iter()
        .filter(|(n, _, _)| *n != "set")
        .map(|(n, args, _)| format!("/{n} {args}").trim_end().to_string())
        .collect();
    usages.extend(
        SET_OPTIONS
            .iter()
            .map(|o| format!("/set {} {}", o.name, o.args).trim_end().to_string()),
    );
    usages
}

/// The hint for `line` with the cursor at its end: the rest of the first command usage it
/// starts.
pub fn command_hint(line: &str) -> Option<CommandHint> {
    if !line.starts_with('/') || line.starts_with("//") {
        return None;
    }
    command_usages()
        .into_iter()
        .find(|usage| usage.starts_with(line) && usage.len() > line.len())
        .map(|usage| {
            let rest = usage[line.len()..].to_string();
            let literal = rest.find(['<', '[']).unwrap_or(rest.len());
            CommandHint {
                complete_up_to: rest[..literal].trim_end().len(),
                display: rest,
            }
        })
}

impl Hinter for ReplHelper {
    type Hint = CommandHint;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<CommandHint> {
        if pos < line.len() {
            return None;
        }
        command_hint(line)
    }
}

/// Runs the interactive `graphersal>` shell against the graph selected on the command line.
/// With a stdin that is not a terminal (`--repl`) it reads the lines without editing, and
/// keeps no history file.
pub fn run(options: Cli) -> Result<()> {
    let interactive = std::io::stdin().is_terminal();
    let opened = match options.graph.open(
        options.create_store,
        options.on_damage,
        options.schema.as_ref(),
    ) {
        Ok(opened) => opened,
        Err(err) => {
            errln!("{}", err.message);
            exit(err.code);
        }
    };
    let arc_graph = opened.graph.clone();
    opened.close_on_signal(None, signals::EXIT_INTERRUPTED);
    // A Store is closed cleanly on every way out of the REPL.
    let mut opened = Some(opened);
    let mut limits = options.script_limits();
    let mut budget = options.budget();
    let mut engine = budget.engine(&limits);
    let mut render = options.render_options();
    let mut scope = budget.scope(arc_graph.clone(), &render);

    let config = Builder::new()
        .auto_add_history(true)
        .history_ignore_space(true)
        .build();

    let helper = ReplHelper {
        catalogue: Catalogue::new(&engine, &scope),
        spelling: render.spelling,
    };

    let mut rl = Editor::with_config(config)?;
    rl.set_helper(Some(helper));

    outln!(
        r#"
This is a CLI for the Rust Graphersal Library
Version: {}

Type /help for the commands; Tab completes steps, tokens and commands
/help <name> describes a step or token (/help has_label, /help P.eq)
Use /exit or CTRL-D to exit; CTRL-C breaks the line input
"#,
        env!("CARGO_PKG_VERSION")
    );

    if interactive {
        let _ = rl.load_history("history.txt");
    }

    let mut is_exit = false;
    let mut default_visualizer = options.format;

    loop {
        // The terminal (or the reader of a redirected stdout) went away: nobody sees the
        // answers, so the session ends and the store closes.
        if !crate::out::stdout_open() {
            break;
        }
        let readline = rl.readline("graphersal> ");
        match readline {
            Ok(line) => {
                match classify(&line) {
                    Input::OldPrefix(hint) => {
                        outln!("{hint}");
                        continue;
                    }
                    Input::Command(name, args) => {
                        let spelling = render.spelling;
                        match name {
                            "exit" => {
                                if let Some(opened) = opened.take() {
                                    opened.close();
                                }
                                exit(0);
                            }
                            "memory" => outln!("{}", memory_report(&arc_graph, args)),
                            "help" => {
                                if let Some(helper) = rl.helper() {
                                    outln!("{}", helper.catalogue.help(args, spelling));
                                }
                            }
                            "set" => {
                                let command = format!("/set {args}");
                                let option = args.split_whitespace().next().unwrap_or("");
                                match option {
                                    "format" => match args.split_whitespace().nth(1) {
                                        Some(raw) => match VisualizeFormat::from_name(raw) {
                                            Some(format) => {
                                                default_visualizer = format;
                                                outln!("Default visualizer set to {format:?}");
                                            }
                                            None => outln!(
                                                "Unknown format: {raw}. Available formats: {}",
                                                VisualizeFormat::names().join(", ")
                                            ),
                                        },
                                        None => outln!(
                                            "Usage: /set format <{}>",
                                            VisualizeFormat::names().join("|")
                                        ),
                                    },
                                    "spelling" => match cli::parse_spelling(
                                        args.trim_start_matches("spelling"),
                                    ) {
                                        Ok(spelling) => {
                                            render = render.clone().with_spelling(spelling);
                                            graphersal::script::set_scope_render_options(
                                                &mut scope, &render,
                                            );
                                            if let Some(helper) = rl.helper_mut() {
                                                helper.spelling = spelling;
                                            }
                                            outln!("Step names are spelled in {spelling}");
                                        }
                                        Err(_) => outln!("Usage: /set spelling <snake|camel>"),
                                    },
                                    "max-operations" | "max-string-size" | "max-array-size"
                                    | "max-map-size" | "max-value-depth" | "safe-limits" => {
                                        match set_script_limit(&command, &limits) {
                                            Ok(updated) => {
                                                limits = updated;
                                                // The value depth and the string limit live in
                                                // the engine and in the traversal presets, so
                                                // the engine and the scope's sources are
                                                // rebuilt; the scope's variables stay.
                                                let string_bytes = match limits.max_string_size {
                                                    0 => i64::MAX,
                                                    n => i64::try_from(n).unwrap_or(i64::MAX),
                                                };
                                                budget = budget
                                                    .with_max_value_depth(Some(
                                                        limits.max_value_depth,
                                                    ))
                                                    .with_max_string_bytes(Some(string_bytes));
                                                engine = budget.engine(&limits);
                                                budget.refresh_scope(
                                                    &mut scope,
                                                    arc_graph.clone(),
                                                    &render,
                                                );
                                                outln!("{}", describe_script_limits(&limits));
                                            }
                                            Err(usage) => outln!("{usage}"),
                                        }
                                    }
                                    "max-rows" | "max-items" | "no-limit" => {
                                        match set_display_limit(&command, &render) {
                                            Ok(updated) => {
                                                render = updated;
                                                graphersal::script::set_scope_render_options(
                                                    &mut scope, &render,
                                                );
                                                outln!("{}", describe_limits(&render));
                                            }
                                            Err(usage) => outln!("{usage}"),
                                        }
                                    }
                                    _ => outln!("{}", set_help()),
                                }
                            }
                            other => {
                                outln!("Unknown command: /{other}. /help lists the commands.")
                            }
                        }
                        continue;
                    }
                    Input::Script => {}
                }

                is_exit = false;
                rl.add_history_entry(line.as_str())?;
                budget.start();
                let res = engine.eval_with_scope::<Dynamic>(&mut scope, line.as_str());

                match res {
                    Ok(res) => {
                        // Statements such as `let x = ...;` evaluate to unit: stay silent
                        // instead of echoing a blank line.
                        let rendered =
                            graphersal::script::render_value(res, default_visualizer, &render);
                        if !rendered.text.is_empty() {
                            outln!("{}", rendered.text);
                        }
                        if let Some(notice) = rendered.notice() {
                            outln!("{notice}");
                        }
                    }
                    Err(err) => outln!(
                        "{}",
                        cli::format_eval_error(
                            &graphersal::script::map_eval_error(err, &limits),
                            &limits
                        )
                    ),
                }
            }
            Err(ReadlineError::Interrupted) => {
                if is_exit {
                    break;
                }
                outln!("Press CTRL-C again to exit");
                is_exit = true;
            }
            Err(ReadlineError::Eof) => {
                if interactive {
                    outln!("CTRL-D");
                }
                break;
            }
            Err(err) => {
                outln!("Error: {err}");
                break;
            }
        }
    }

    if let Some(opened) = opened.take() {
        opened.close();
    }
    if interactive {
        rl.append_history("history.txt")?;
    }
    Ok(())
}

/// Applies a `/set max-rows <n>`, `/set max-items <n>` or `/set no-limit` command to the current
/// display limits. Setting one limit after `/set no-limit` turns limits back on, with the other
/// one at its default.
fn set_display_limit(
    command: &str,
    current: &RenderOptions,
) -> std::result::Result<RenderOptions, String> {
    let parts: Vec<&str> = command.split_ascii_whitespace().collect();
    let usage = "Usage: /set max-rows <n> | /set max-items <n> | /set no-limit   (n >= 1)";
    let limit = |raw: Option<&&str>| match raw.and_then(|r| r.parse::<usize>().ok()) {
        Some(n) if n >= 1 => Ok(n),
        _ => Err(usage.to_string()),
    };
    let rows = current.max_rows.unwrap_or(cli::DEFAULT_MAX_ROWS);
    let items = current.max_items.unwrap_or(cli::DEFAULT_MAX_ITEMS);
    match parts.get(1).copied() {
        Some("no-limit") if parts.len() == 2 => {
            Ok(RenderOptions::unlimited().with_spelling(current.spelling))
        }
        Some("max-rows") if parts.len() == 3 => Ok(RenderOptions::new()
            .with_max_rows(limit(parts.get(2))?)
            .with_max_items(items)
            .with_spelling(current.spelling)),
        Some("max-items") if parts.len() == 3 => Ok(RenderOptions::new()
            .with_max_rows(rows)
            .with_max_items(limit(parts.get(2))?)
            .with_spelling(current.spelling)),
        _ => Err(usage.to_string()),
    }
}

/// Applies a `/set max-operations <n>` (or `max-string-size`, `max-array-size`, `max-map-size`,
/// `max-value-depth`) or `/set safe-limits` command to the current script limits. `0` means
/// unlimited.
fn set_script_limit(
    command: &str,
    current: &ScriptLimits,
) -> std::result::Result<ScriptLimits, String> {
    let usage = "Usage: /set max-operations <n> | /set max-string-size <n> | /set max-array-size <n> | \
                 /set max-map-size <n> | /set max-value-depth <n> | /set safe-limits   (n >= 0, 0 means \
                 unlimited; for max-value-depth the default, 128)";
    let parts: Vec<&str> = command.split_ascii_whitespace().collect();
    let number = |raw: Option<&&str>| {
        raw.and_then(|r| r.parse::<u64>().ok())
            .ok_or_else(|| usage.to_string())
    };
    let size = |raw: Option<&&str>| number(raw).map(|n| usize::try_from(n).unwrap_or(usize::MAX));
    let mut updated = current.clone();
    match (parts.get(1).copied(), parts.len()) {
        (Some("safe-limits"), 2) => updated = ScriptLimits::default(),
        (Some("max-operations"), 3) => updated.max_operations = number(parts.get(2))?,
        (Some("max-string-size"), 3) => updated.max_string_size = size(parts.get(2))?,
        (Some("max-array-size"), 3) => updated.max_array_size = size(parts.get(2))?,
        (Some("max-map-size"), 3) => updated.max_map_size = size(parts.get(2))?,
        (Some("max-value-depth"), 3) => updated.max_value_depth = size(parts.get(2))?,
        _ => return Err(usage.to_string()),
    }
    Ok(updated)
}

/// Describes the current script limits after a `/set` command.
fn describe_script_limits(limits: &ScriptLimits) -> String {
    let show = |n: u64| {
        if n == 0 {
            "unlimited".to_string()
        } else {
            n.to_string()
        }
    };
    let depth = if limits.max_value_depth == 0 {
        ScriptLimits::default().max_value_depth
    } else {
        limits.max_value_depth
    };
    format!(
        "Script limits: {} operations, {} string bytes, {} array items, {} map entries, {} value \
         depth",
        show(limits.max_operations),
        show(limits.max_string_size as u64),
        show(limits.max_array_size as u64),
        show(limits.max_map_size as u64),
        depth,
    )
}

/// Describes the current display limits after a `/set` command.
fn describe_limits(render: &RenderOptions) -> String {
    match (render.max_rows, render.max_items) {
        (Some(rows), Some(items)) => format!("Display limits: {rows} rows, {items} nested items"),
        _ => "Display limits removed: results are shown completely".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalogue of a session on the modern graph.
    fn catalogue() -> Catalogue {
        let cli = Cli::parse(Vec::<String>::new()).unwrap();
        let opened = cli.graph.open(false, None, None).unwrap();
        let budget = cli.budget();
        let engine = budget.engine(&cli.script_limits());
        let scope = budget.scope(opened.graph.clone(), &cli.render_options());
        Catalogue::new(&engine, &scope)
    }

    fn displays(catalogue: &Catalogue, line: &str, spelling: Spelling) -> Vec<String> {
        catalogue
            .complete(line, line.len(), spelling)
            .1
            .into_iter()
            .map(|p| p.display)
            .collect()
    }

    #[test]
    fn steps_complete_after_g_and_in_the_session_spelling() {
        let c = catalogue();
        let snake = displays(&c, "g.", Spelling::Snake);
        assert!(snake.iter().any(|n| n == "has_label"), "{snake:?}");
        assert!(snake.iter().any(|n| n == "v"), "{snake:?}");
        assert!(!snake.iter().any(|n| n == "hasLabel"), "{snake:?}");
        let camel = displays(&c, "g.", Spelling::Camel);
        assert!(camel.iter().any(|n| n == "hasLabel"), "{camel:?}");
        assert!(camel.iter().any(|n| n == "V"), "{camel:?}");
        assert!(!camel.iter().any(|n| n == "has_label"), "{camel:?}");
        // The replaced word starts after the dot; a step with arguments opens its call.
        let (start, pairs) = c.complete("g.V().has", 9, Spelling::Snake);
        assert_eq!(start, 6);
        let names: Vec<&str> = pairs.iter().map(|p| p.display.as_str()).collect();
        assert!(
            names.contains(&"has") && names.contains(&"has_label"),
            "{names:?}"
        );
        assert!(names.iter().all(|n| n.starts_with("has")), "{names:?}");
        let has_label = pairs.iter().find(|p| p.display == "has_label").unwrap();
        assert_eq!(has_label.replacement, "has_label(");
        // A spelling the session does not use still completes when it is the only match.
        assert!(displays(&c, "g.V().hasL", Spelling::Snake).contains(&"hasLabel".to_string()));
        // `__.` completes steps too.
        assert!(displays(&c, "__.ou", Spelling::Snake).contains(&"out".to_string()));
    }

    #[test]
    fn token_classes_complete_their_values_and_functions() {
        let c = catalogue();
        let p = displays(&c, "g.V().has(\"age\", P.", Spelling::Snake);
        assert!(
            p.iter().any(|n| n == "gt") && p.iter().any(|n| n == "within"),
            "{p:?}"
        );
        assert!(!p.iter().any(|n| n == "has_label"), "{p:?}");
        assert_eq!(
            displays(&c, "Order.", Spelling::Snake),
            ["asc", "desc", "shuffle"]
        );
        assert_eq!(displays(&c, "T.l", Spelling::Snake), ["label"]);
        assert!(displays(&c, "GType::LO", Spelling::Snake).contains(&"LONG".to_string()));
        assert!(displays(&c, "TextP.", Spelling::Snake).contains(&"containing".to_string()));
        // The start of an expression: globals, keywords and free functions.
        let globals = displays(&c, "", Spelling::Snake);
        for name in ["g", "__", "P", "Order", "let", "jpath"] {
            assert!(globals.iter().any(|n| n == name), "{name}: {globals:?}");
        }
        // Inside a string literal nothing completes.
        assert!(displays(&c, "g.V().has(\"na", Spelling::Snake).is_empty());
    }

    #[test]
    fn memory_prints_the_report_summary() {
        let graph = graphersal::storage::Graph::new(graphersal::TraversalGraph::tinkerpop_modern());
        let full = memory_report(&graph, "");
        assert!(full.starts_with("Memory report at commit 0:"), "{full}");
        assert!(full.contains("vertex person $.name"), "{full}");
        let sampled = memory_report(&graph, " 3 ");
        assert!(sampled.contains("Sampled:"), "{sampled}");
        assert!(memory_report(&graph, "lots").starts_with("Usage: /memory"));
        assert!(memory_report(&graph, "0").starts_with("Usage: /memory"));
    }

    #[test]
    fn slash_commands_and_their_arguments_complete() {
        let c = catalogue();
        assert_eq!(
            displays(&c, "/", Spelling::Snake),
            ["/exit", "/help", "/memory", "/set"]
        );
        assert_eq!(displays(&c, "/s", Spelling::Snake), ["/set"]);
        let set = displays(&c, "/set ", Spelling::Snake);
        for option in ["format", "max-rows", "no-limit", "spelling", "safe-limits"] {
            assert!(set.iter().any(|n| n == option), "{option}: {set:?}");
        }
        assert_eq!(displays(&c, "/set max-r", Spelling::Snake), ["max-rows"]);
        assert_eq!(
            displays(&c, "/set spelling ", Spelling::Snake),
            ["camel", "snake"]
        );
        assert!(displays(&c, "/set format ", Spelling::Snake).contains(&"json".to_string()));
        let (start, _) = c.complete("/set spelling ca", 16, Spelling::Snake);
        assert_eq!(start, 14);
        assert!(displays(&c, "/help has_l", Spelling::Snake).contains(&"has_label".to_string()));
        assert!(displays(&c, "/help hasL", Spelling::Camel).contains(&"hasLabel".to_string()));
        assert!(displays(&c, "/help P.", Spelling::Snake).contains(&"P.eq".to_string()));
        // A `//` comment is a script, not a command.
        assert_eq!(classify("// note"), Input::Script);
    }

    #[test]
    fn help_reads_the_engine_docs_in_either_spelling() {
        let c = catalogue();
        let snake = c.help("has_label", Spelling::Snake);
        assert!(snake.starts_with("has_label  (step"), "{snake}");
        assert!(snake.contains("also: hasLabel"), "{snake}");
        assert!(snake.contains("Keeps elements whose label"), "{snake}");
        assert!(snake.contains("Example: `"), "{snake}");
        assert_eq!(c.help("hasLabel", Spelling::Snake), snake);
        assert!(
            c.help("hasLabel", Spelling::Camel)
                .starts_with("hasLabel  (step")
        );
        assert!(
            c.help("P.eq", Spelling::Snake)
                .starts_with("P.eq  (function of P)")
        );
        let order = c.help("Order", Spelling::Snake);
        assert!(order.contains("Order.desc"), "{order}");
        assert!(
            c.help("Order.desc", Spelling::Snake)
                .contains("token class Order")
        );
        let unknown = c.help("nosuchstep", Spelling::Snake);
        assert!(unknown.starts_with("No help for `nosuchstep`"), "{unknown}");
        let list = c.help("", Spelling::Snake);
        for command in [
            "/help <name>",
            "/set max-rows <n>",
            "/set spelling",
            "/exit",
        ] {
            assert!(list.contains(command), "{command}: {list}");
        }
        assert!(c.help("/set", Spelling::Snake).contains("/set no-limit"));
        assert!(c.help("steps", Spelling::Camel).contains("groupCount"));
        assert!(c.help("tokens", Spelling::Snake).contains("Order"));
    }

    #[test]
    fn the_old_colon_prefix_points_to_the_slash_form() {
        assert_eq!(
            classify(":help has_label"),
            Input::OldPrefix(
                "Commands start with `/`: use `/help has_label` (/help lists them)".into()
            )
        );
        assert_eq!(
            classify("  /set max-rows 5 "),
            Input::Command("set", "max-rows 5")
        );
        assert_eq!(classify("g.V().count();"), Input::Script);
    }

    #[test]
    fn command_hints_complete_only_the_literal_part() {
        let hint = command_hint("/set max-r").unwrap();
        assert_eq!(hint.display(), "ows <n>");
        assert_eq!(hint.completion(), Some("ows"));
        assert!(command_hint("g.V()").is_none());
        assert!(command_hint("/set max-rows 5").is_none());
    }

    #[test]
    fn set_commands_change_the_display_limits() {
        let defaults = Cli::parse(Vec::<String>::new()).unwrap().render_options();
        let rows = set_display_limit("/set max-rows 20", &defaults).unwrap();
        assert_eq!((rows.max_rows, rows.max_items), (Some(20), Some(100)));
        let items = set_display_limit("/set max-items 3", &rows).unwrap();
        assert_eq!((items.max_rows, items.max_items), (Some(20), Some(3)));
        let none = set_display_limit("/set no-limit", &items).unwrap();
        assert!(!none.is_bounded());
        // A limit after `/set no-limit` turns limits back on, the other one at its default.
        let back = set_display_limit("/set max-rows 5", &none).unwrap();
        assert_eq!((back.max_rows, back.max_items), (Some(5), Some(100)));
        assert!(set_display_limit("/set max-rows 0", &defaults).is_err());
        assert!(set_display_limit("/set max-rows", &defaults).is_err());
        assert!(set_display_limit("/set no-limit now", &defaults).is_err());
    }

    #[test]
    fn set_script_limit_updates_one_field_and_safe_limits_resets() {
        let unlimited = ScriptLimits::unlimited();
        let ops = set_script_limit("/set max-operations 500", &unlimited).unwrap();
        assert_eq!(ops.max_operations, 500);
        assert_eq!(ops.max_array_size, 0);
        let array = set_script_limit("/set max-array-size 7", &ops).unwrap();
        assert_eq!((array.max_operations, array.max_array_size), (500, 7));
        let safe = set_script_limit("/set safe-limits", &array).unwrap();
        assert_eq!(safe, ScriptLimits::default());
        assert!(set_script_limit("/set max-operations", &safe).is_err());
        assert!(set_script_limit("/set max-operations x", &safe).is_err());
        assert!(describe_script_limits(&unlimited).contains("unlimited operations"));
    }
}
