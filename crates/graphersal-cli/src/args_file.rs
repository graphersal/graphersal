//! `--args-file FILE` (and its synonym `@FILE`): a JSON file that lists command-line arguments.
//!
//! The file is one JSON object whose keys are the LONG flag names without dashes. It is
//! translated into a synthetic argument list that is PREPENDED to the real arguments, so the
//! ordinary parser in `cli.rs` stays the one source of truth: a scalar flag of the command line
//! replaces the file's (the last one wins), a repeatable flag concatenates (file first), and
//! the validation of conflicting modes runs on the merged list.
//!
//! Unlike the `--ai` file (unknown keys warn), an unknown key here is a hard error: a flag that
//! is silently dropped could turn off `mcp-read-only` or `mcp-token` without anyone noticing.
//! Only a key starting with `_` is a silent comment.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// How the value of a key is written.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `true` adds the flag, `false` leaves it out.
    Switch,
    /// A string or a number, as it would be typed.
    Value,
    /// A path, resolved against the directory of the args file when relative.
    Path,
    /// `--graph`: a path unless it names a sample graph.
    Graph,
    /// A string or an array of strings, one occurrence of the repeatable flag each.
    Repeat,
    /// `--embedder NAME=PATH|REPO`: repeatable, the PATH part resolved like a path.
    Embedder,
}

/// Every key: the long flag name and how its value is written. `help`, `version` and
/// `args-file` itself are not keys.
const KEYS: &[(&str, Kind)] = &[
    ("graph", Kind::Graph),
    ("schema", Kind::Path),
    ("schema-mode", Kind::Value),
    ("create-store", Kind::Switch),
    ("on-damage", Kind::Value),
    ("eval", Kind::Repeat),
    ("in", Kind::Switch),
    ("repl", Kind::Switch),
    ("format", Kind::Value),
    ("timeout", Kind::Value),
    ("max-rows", Kind::Value),
    ("max-items", Kind::Value),
    ("no-limit", Kind::Switch),
    ("spelling", Kind::Value),
    ("safe-limits", Kind::Switch),
    ("max-operations", Kind::Value),
    ("max-string-size", Kind::Value),
    ("max-array-size", Kind::Value),
    ("max-map-size", Kind::Value),
    ("max-traversers", Kind::Value),
    ("max-value-depth", Kind::Value),
    ("memory-limit", Kind::Value),
    ("memory-headroom", Kind::Value),
    ("server", Kind::Switch),
    ("port", Kind::Value),
    ("host", Kind::Value),
    ("ui-dir", Kind::Path),
    ("mcp", Kind::Switch),
    ("mcp-read-only", Kind::Switch),
    ("mcp-token", Kind::Value),
    ("mcp-query-tools", Kind::Switch),
    ("mcp-instructions", Kind::Path),
    ("mcp-max-chars", Kind::Value),
    ("ai", Kind::Path),
    ("embedder", Kind::Embedder),
    ("default-embedder", Kind::Value),
];

/// The flags that take the next argument as their value (long and short spellings), so the
/// scan for `--args-file` never mistakes a script such as `-e @x` for the file.
const VALUE_FLAGS: &[&str] = &[
    "-g",
    "--graph",
    "--schema",
    "--schema-mode",
    "--on-damage",
    "-e",
    "--eval",
    "-f",
    "--format",
    "-t",
    "--timeout",
    "--max-rows",
    "--max-items",
    "--spelling",
    "--max-operations",
    "--max-string-size",
    "--max-array-size",
    "--max-map-size",
    "--max-traversers",
    "--max-value-depth",
    "--memory-limit",
    "--memory-headroom",
    "--port",
    "--host",
    "--ui-dir",
    "--mcp-token",
    "--mcp-instructions",
    "--mcp-max-chars",
    "--ai",
    "--embedder",
    "--default-embedder",
];

const EXAMPLE: &str =
    r#"{"graph": "modern", "server": true, "port": 8080, "embedder": ["a=/models/a"]}"#;

/// The Levenshtein distance of two keys (small inputs, no dependency).
pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1)
                .min(row[j] + 1)
                .min(diagonal + usize::from(ca != *cb));
            diagonal = above;
        }
    }
    row[b.len()]
}

/// Replaces the `--args-file FILE` / `--args-file=FILE` / `@FILE` argument (at most one) by
/// the arguments of the file, placed BEFORE the remaining real arguments.
pub fn expand(args: Vec<String>) -> Result<Vec<String>, String> {
    let mut file: Option<String> = None;
    let mut rest = Vec::with_capacity(args.len());
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        let given = if arg == "--args-file" {
            Some(iter.next().ok_or("Option --args-file requires a value")?)
        } else if let Some(path) = arg.strip_prefix("--args-file=") {
            Some(path.to_string())
        } else if arg.len() > 1 && arg.starts_with('@') {
            Some(arg[1..].to_string())
        } else {
            None
        };
        match given {
            Some(path) => {
                if file.replace(path).is_some() {
                    return Err("--args-file may appear only once (and @FILE is the same \
                                option)\nHelp: list everything in one file, or repeat \
                                only the flags on the command line"
                        .to_string());
                }
            }
            None => {
                // A value of a flag is never an args-file: copy it unread.
                let takes_value = VALUE_FLAGS.contains(&arg.as_str());
                rest.push(arg);
                if takes_value && let Some(value) = iter.next() {
                    rest.push(value);
                }
            }
        }
    }
    let Some(file) = file else {
        return Ok(rest);
    };
    let mut args = load(&file)?;
    args.extend(rest);
    Ok(args)
}

/// Reads the file and translates it into `--key=value` arguments.
fn load(file: &str) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(file).map_err(|err| {
        format!(
            "--args-file {file}: cannot read the file ({err})\nHelp: the argument is the path \
             of a JSON file, e.g. {EXAMPLE}"
        )
    })?;
    let base = Path::new(file).parent().unwrap_or(Path::new(""));
    translate(&text, base).map_err(|why| {
        format!(
            "--args-file {file}: {why}\nHelp: the file is one JSON object whose keys are the \
             long flag names without dashes, e.g. {EXAMPLE}; a key starting with \"_\" is a \
             comment"
        )
    })
}

/// The arguments of the args file `text`; relative paths resolve against `base`.
fn translate(text: &str, base: &Path) -> Result<Vec<String>, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|err| format!("not valid JSON ({err})"))?;
    let Value::Object(map) = value else {
        return Err("expected a JSON object".to_string());
    };
    check_keys(&map)?;
    let mut out = Vec::new();
    // The order of the file (alphabetical in serde_json's map) is irrelevant except for the
    // repeatable flags, which keep the order of their own array.
    for (key, value) in &map {
        let Some((_, kind)) = KEYS.iter().find(|(name, _)| name == key) else {
            continue; // a `_` comment
        };
        match kind {
            Kind::Switch => match value {
                Value::Bool(true) => out.push(format!("--{key}")),
                Value::Bool(false) => {}
                other => return Err(wrong_type(key, "true or false", other)),
            },
            Kind::Value => out.push(format!("--{key}={}", scalar(key, value)?)),
            Kind::Path => out.push(format!("--{key}={}", resolve(&scalar(key, value)?, base))),
            Kind::Graph => {
                let spec = scalar(key, value)?;
                let spec = if is_sample_graph(&spec) {
                    spec
                } else {
                    resolve(&spec, base)
                };
                out.push(format!("--{key}={spec}"));
            }
            Kind::Repeat | Kind::Embedder => {
                let items = match value {
                    Value::Array(items) => items.iter().collect(),
                    single => vec![single],
                };
                for item in items {
                    let item = scalar(key, item)?;
                    let item = if *kind == Kind::Embedder {
                        resolve_embedder(&item, base)
                    } else {
                        item
                    };
                    out.push(format!("--{key}={item}"));
                }
            }
        }
    }
    Ok(out)
}

/// Rejects a key that is no flag: `args-file` itself, a near-typo (with a hint) or any other.
fn check_keys(map: &Map<String, Value>) -> Result<(), String> {
    let names = || {
        KEYS.iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(", ")
    };
    for key in map.keys() {
        if key.starts_with('_') || KEYS.iter().any(|(name, _)| name == key) {
            continue;
        }
        if key == "args-file" {
            return Err("key \"args-file\": an args file cannot include another one".to_string());
        }
        if key == "help" || key == "version" {
            return Err(format!(
                "key \"{key}\": --{key} is not a setting; run graphersal --{key} instead"
            ));
        }
        return Err(
            match KEYS
                .iter()
                .map(|(name, _)| *name)
                .find(|known| edit_distance(key, known) <= 2)
            {
                Some(near) => format!(
                    "unknown key \"{key}\": did you mean \"{near}\"? The keys are: {}",
                    names()
                ),
                None => format!("unknown key \"{key}\". The keys are: {}", names()),
            },
        );
    }
    Ok(())
}

/// A string or a number as it would be typed; never echoes the value.
fn scalar(key: &str, value: &Value) -> Result<String, String> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Number(number) => Ok(number.to_string()),
        other => Err(wrong_type(key, "a string or a number", other)),
    }
}

fn wrong_type(key: &str, expected: &str, got: &Value) -> String {
    let got = match got {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    };
    format!("key \"{key}\": expected {expected}, got {got}")
}

/// `--graph` values that are no path.
fn is_sample_graph(spec: &str) -> bool {
    matches!(
        spec.to_ascii_lowercase().as_str(),
        "modern"
            | "tinkerpop_modern"
            | "tinkerpop-modern"
            | "empty"
            | "new"
            | "large"
            | "large_110k"
            | "tree"
            | "file_tree"
            | "file-tree"
    )
}

/// A relative path joined to `base`; an absolute one (or an empty base) stays as written.
fn resolve(path: &str, base: &Path) -> String {
    let given = Path::new(path);
    if given.is_absolute() || base.as_os_str().is_empty() {
        return path.to_string();
    }
    let joined: PathBuf = base.join(given);
    joined.to_string_lossy().into_owned()
}

/// `NAME=PATH|REPO`: the part after the first `=` is a path (resolved against `base`) when it
/// is written as one (`./`, `../`) or exists there; otherwise a model repository, as it is.
fn resolve_embedder(spec: &str, base: &Path) -> String {
    let Some((name, target)) = spec.split_once('=') else {
        return spec.to_string();
    };
    let written_as_path = target.starts_with("./") || target.starts_with("../");
    if written_as_path || base.join(target).exists() {
        format!("{name}={}", resolve(target, base))
    } else {
        spec.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(text: &str) -> Result<Vec<String>, String> {
        translate(text, Path::new("/etc/g"))
    }

    #[test]
    fn keys_become_flags() {
        let args = tr(
            r#"{"server": true, "mcp": false, "port": 0, "graph": "modern",
                          "eval": ["1", "2"], "_note": "x"}"#,
        )
        .unwrap();
        assert_eq!(
            args,
            [
                "--eval=1",
                "--eval=2",
                "--graph=modern",
                "--port=0",
                "--server"
            ]
        );
    }

    #[test]
    fn relative_paths_resolve_against_the_file() {
        let args = tr(
            r#"{"graph": "data/g.json", "schema": "/abs/s.json", "ai": "ai.json",
                          "ui-dir": "ui", "mcp-instructions": "n.md",
                          "embedder": "a=./m"}"#,
        )
        .unwrap();
        // Joined with the platform's separator (`\` on Windows).
        let under = |rel: &str| Path::new("/etc/g").join(rel).display().to_string();
        assert!(
            args.contains(&format!("--graph={}", under("data/g.json"))),
            "{args:?}"
        );
        assert!(args.contains(&"--schema=/abs/s.json".to_string()));
        assert!(args.contains(&format!("--ai={}", under("ai.json"))));
        assert!(args.contains(&format!("--ui-dir={}", under("ui"))));
        assert!(args.contains(&format!("--mcp-instructions={}", under("n.md"))));
        assert!(args.contains(&format!("--embedder=a={}", under("./m"))));
        // a repository name stays one
        let args = tr(r#"{"embedder": "p=minishlab/potion-base-8M"}"#).unwrap();
        assert_eq!(args, ["--embedder=p=minishlab/potion-base-8M"]);
    }

    #[test]
    fn errors_name_the_key_and_never_echo_values() {
        let err = tr(r#"{"mcp-token": true}"#).unwrap_err();
        assert!(
            err.contains("\"mcp-token\"") && err.contains("a boolean"),
            "{err}"
        );
        let err = tr(r#"{"port": "secret-value", "server": 1}"#).unwrap_err();
        assert!(
            err.contains("\"server\"") && !err.contains("secret-value"),
            "{err}"
        );
        assert!(tr(r#"{"graph": ["a"]}"#).is_err());
        assert!(tr("[1]").unwrap_err().contains("JSON object"));
        assert!(tr("{").unwrap_err().contains("not valid JSON"));
    }

    #[test]
    fn unknown_keys_are_errors_with_a_hint() {
        let err = tr(r#"{"sever": true}"#).unwrap_err();
        assert!(err.contains("did you mean \"server\"?"), "{err}");
        let err = tr(r#"{"frobnicate": true}"#).unwrap_err();
        assert!(
            err.contains("unknown key") && !err.contains("did you mean"),
            "{err}"
        );
        assert!(
            tr(r#"{"args-file": "x.json"}"#)
                .unwrap_err()
                .contains("another")
        );
        assert!(tr(r#"{"_sever": true}"#).unwrap().is_empty());
    }

    #[test]
    fn expansion_prepends_and_scans_only_flags() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // `-e @x` is a script, not an args file
        assert_eq!(
            expand(args(&["-e", "@x", "--port", "1"])).unwrap(),
            args(&["-e", "@x", "--port", "1"])
        );
        assert!(expand(args(&["--args-file"])).is_err());
        let err = expand(args(&["--args-file=a.json", "@b.json"])).unwrap_err();
        assert!(err.contains("only once"), "{err}");
        let err = expand(args(&["@/nonexistent/a.json"])).unwrap_err();
        assert!(err.contains("--args-file /nonexistent/a.json"), "{err}");
    }

    #[test]
    fn every_key_is_a_documented_flag() {
        for (key, _) in KEYS {
            assert!(crate::cli::HELP.contains(&format!("--{key}")), "{key}");
        }
    }
}
