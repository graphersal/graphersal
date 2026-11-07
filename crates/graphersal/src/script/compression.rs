//! Compression rules in the DSL (book page "Compressed Properties"): plain calls on the source,
//! not steps, in both spellings: `g.define_compression(#{..})`, `g.compressions()`,
//! `g.drop_compression(name)`, `g.recompress(name)` and the read-only
//! `g.estimate_compression(#{..})`.

use super::saved_query::{fail, graph_fail, optional_text};
use super::*;
use crate::error::ScriptError;
use crate::graph::compression::{Codec, CompressionDefinition};
use crate::graph::compression_estimate::{
    CompressionCandidate, CompressionEstimate, CompressionEstimateRequest,
};
use crate::graph::definition::{Definition, DefinitionKind};
use crate::graph::{AccessRequest, Action, ElementKind, Resource};

/// The keys `define_compression` accepts.
const KEYS: [&str; 7] = [
    "name",
    "label",
    "path",
    "element",
    "codec",
    "min_bytes",
    "dictionary",
];

/// Why a rule of a known codec is not applied: the storage applies no compression rules.
const STORAGE_APPLIES_NONE: &str = "this storage keeps the rule in its catalog but does not \
                                    apply compression rules; values stay plain";

/// One rule as a map: name, element, label, path, codec, min_bytes, dictionary, served (with
/// `reason` when it is not applied: a codec, element kind or path of a newer version, or a
/// storage without compressed properties) and its current stats (compressed_values,
/// plain_bytes, stored_bytes, dictionary_bytes, distinct_values).
pub(crate) fn rule_entry(
    definition: &Definition,
    stats: Option<crate::graph::CompressionStats>,
) -> Option<rhai::Map> {
    let rule = definition.as_compression()?;
    let mut map = rhai::Map::new();
    map.insert("name".into(), definition.name().to_string().into());
    map.insert("element".into(), rule.element_name().to_string().into());
    map.insert("label".into(), rule.label().to_string().into());
    map.insert("path".into(), rule.path_text().into());
    map.insert("codec".into(), rule.codec_name().to_string().into());
    map.insert(
        "min_bytes".into(),
        Dynamic::from_int(rule.min_bytes() as i64),
    );
    map.insert("dictionary".into(), Dynamic::from_bool(rule.dictionary()));
    map.insert("served".into(), Dynamic::from_bool(stats.is_some()));
    if stats.is_none() {
        let reason = rule
            .unserved_reason()
            .unwrap_or_else(|| STORAGE_APPLIES_NONE.to_string());
        map.insert("reason".into(), reason.into());
    }
    let stats = stats.unwrap_or_default();
    let int = |n: u64| Dynamic::from_int(i64::try_from(n).unwrap_or(i64::MAX));
    map.insert("compressed_values".into(), int(stats.compressed_values));
    map.insert("plain_bytes".into(), int(stats.plain_bytes));
    map.insert("stored_bytes".into(), int(stats.stored_bytes));
    map.insert("dictionary_bytes".into(), int(stats.dictionary_bytes));
    map.insert("distinct_values".into(), int(stats.distinct_values));
    Some(map)
}

/// The keys `estimate_compression` accepts.
const ESTIMATE_KEYS: [&str; 6] = [
    "label",
    "path",
    "element",
    "codec",
    "min_bytes",
    "dictionary",
];

/// A count as a Rhai integer (saturating).
fn int(n: u64) -> Dynamic {
    Dynamic::from_int(i64::try_from(n).unwrap_or(i64::MAX))
}

/// A float or `()`.
fn float_or_unit(value: Option<f64>) -> Dynamic {
    value.map_or(Dynamic::UNIT, Dynamic::from_float)
}

/// An estimate as a map (book page "Compressed Properties", section "Estimating the gain"):
/// element, label, path, elements, values_found, string_values, string_bytes, sampled,
/// measured_values, histogram, candidates, best, current, warnings, elapsed_ms.
pub(crate) fn estimate_entry(estimate: &CompressionEstimate) -> rhai::Map {
    let mut map = rhai::Map::new();
    map.insert(
        "element".into(),
        estimate.element.as_str().to_string().into(),
    );
    map.insert("label".into(), estimate.label.clone().into());
    map.insert("path".into(), estimate.path.clone().into());
    map.insert("elements".into(), int(estimate.elements));
    map.insert("values_found".into(), int(estimate.values_found));
    map.insert("string_values".into(), int(estimate.string_values));
    map.insert("string_bytes".into(), int(estimate.string_bytes));
    map.insert("sampled".into(), Dynamic::from_bool(estimate.sampled));
    map.insert("measured_values".into(), int(estimate.measured_values));
    let histogram: rhai::Array = estimate
        .histogram
        .iter()
        .map(|bucket| {
            let mut entry = rhai::Map::new();
            entry.insert("min_bytes".into(), int(bucket.min_bytes));
            entry.insert(
                "max_bytes".into(),
                bucket.max_bytes.map_or(Dynamic::UNIT, int),
            );
            entry.insert("values".into(), int(bucket.values));
            entry.insert("bytes".into(), int(bucket.bytes));
            Dynamic::from_map(entry)
        })
        .collect();
    map.insert("histogram".into(), histogram.into());
    let candidates: rhai::Array = estimate
        .candidates
        .iter()
        .map(|c| {
            let mut entry = rhai::Map::new();
            entry.insert("codec".into(), c.candidate.codec.name().to_string().into());
            entry.insert("min_bytes".into(), int(u64::from(c.candidate.min_bytes)));
            entry.insert(
                "dictionary".into(),
                Dynamic::from_bool(c.candidate.dictionary),
            );
            entry.insert("compressed_values".into(), int(c.compressed_values));
            entry.insert("plain_bytes".into(), int(c.plain_bytes));
            entry.insert("stored_bytes".into(), int(c.stored_bytes));
            entry.insert("dictionary_bytes".into(), int(c.dictionary_bytes));
            entry.insert("distinct_values".into(), int(c.distinct_values));
            entry.insert("saved_bytes".into(), Dynamic::from_int(c.saved_bytes));
            entry.insert("ratio".into(), float_or_unit(c.ratio()));
            entry.insert("current".into(), Dynamic::from_bool(c.current));
            entry.insert(
                "encode_bytes_per_sec".into(),
                float_or_unit(c.encode_bytes_per_sec),
            );
            entry.insert(
                "decode_bytes_per_sec".into(),
                float_or_unit(c.decode_bytes_per_sec),
            );
            Dynamic::from_map(entry)
        })
        .collect();
    map.insert("candidates".into(), candidates.into());
    map.insert(
        "best".into(),
        estimate.best.map_or(Dynamic::UNIT, |at| int(at as u64)),
    );
    let current = estimate.current.as_ref().map_or(Dynamic::UNIT, |current| {
        let mut entry = rhai::Map::new();
        entry.insert("name".into(), current.name.clone().into());
        entry.insert(
            "codec".into(),
            current.rule.codec().name().to_string().into(),
        );
        entry.insert("min_bytes".into(), int(u64::from(current.rule.min_bytes())));
        entry.insert(
            "dictionary".into(),
            Dynamic::from_bool(current.rule.dictionary()),
        );
        entry.insert(
            "compressed_values".into(),
            int(current.stats.compressed_values),
        );
        entry.insert("plain_bytes".into(), int(current.stats.plain_bytes));
        entry.insert("stored_bytes".into(), int(current.stats.stored_bytes));
        entry.insert(
            "dictionary_bytes".into(),
            int(current.stats.dictionary_bytes),
        );
        entry.insert("distinct_values".into(), int(current.stats.distinct_values));
        Dynamic::from_map(entry)
    });
    map.insert("current".into(), current);
    let warnings: rhai::Array = estimate
        .warnings
        .iter()
        .map(|warning| {
            let mut entry = rhai::Map::new();
            entry.insert("code".into(), warning.code().to_string().into());
            entry.insert("message".into(), warning.message(estimate).into());
            Dynamic::from_map(entry)
        })
        .collect();
    map.insert("warnings".into(), warnings.into());
    map.insert(
        "elapsed_ms".into(),
        Dynamic::from_float(estimate.elapsed_micros as f64 / 1000.0),
    );
    map
}

/// The key of `define_compressions`' short form (one rule per path), besides [`KEYS`].
const PATHS_KEY: &str = "paths";

/// A path value (`"details.cv"` or a `jpath(..)`) as a jpath; `None` when it is neither.
fn path_of(value: &Dynamic) -> Option<Result<crate::value::JsonPath, Box<EvalAltResult>>> {
    if let Some(path) = value.read_lock::<crate::value::JsonPath>() {
        Some(Ok(path.clone()))
    } else if value.is_string() {
        Some(
            value
                .to_string()
                .parse()
                .map_err(|err: crate::TraverserError| script_error(Box::new(err))),
        )
    } else {
        None
    }
}

/// The name of a rule of `paths` (`define_compressions`): `<label>_<path>` with the path's
/// names joined by `_`, `_edge` added for an edge, characters a name may not hold as `_`.
fn generated_name(element: ElementKind, label: &str, path: &crate::value::JsonPath) -> String {
    let text = path.to_string();
    let text = text.strip_prefix("$.").unwrap_or(&text);
    crate::graph::advisor::suggested_name(element, label, text)
}

/// The rule definitions of one spec map (module docs; `define_compression`'s keys, and with
/// `paths_allowed` also `paths: [..]` instead of `name` + `path`, one rule per path). Each
/// definition is validated.
fn rule_definitions(
    operation: &str,
    spec: Dynamic,
    paths_allowed: bool,
) -> Result<Vec<Definition>, Box<EvalAltResult>> {
    let actual = type_name_of(&spec);
    let mismatch = |expected: String, actual: String| {
        fail(ScriptError::ArgumentMismatch {
            step_name: operation.to_string(),
            expected,
            actual,
        })
    };
    let Some(mut spec) = spec.try_cast::<rhai::Map>() else {
        return Err(mismatch(
            if paths_allowed {
                "each rule as a map #{name, label, path, element?, codec?, min_bytes?, \
                 dictionary?} or #{label, paths: [..], ..}"
                    .into()
            } else {
                "a map #{name, label, path, element?, codec?, min_bytes?, dictionary?}".into()
            },
            actual,
        ));
    };
    let allowed = |key: &str| KEYS.contains(&key) || (paths_allowed && key == PATHS_KEY);
    if let Some(unknown) = spec.keys().find(|key| !allowed(key.as_str())) {
        let mut keys = KEYS.join(", ");
        if paths_allowed {
            keys.push_str(", paths");
        }
        return Err(mismatch(
            format!("only the keys {keys}"),
            format!("the key '{unknown}'"),
        ));
    }
    let mut take = |key: &str| spec.remove(key).unwrap_or(Dynamic::UNIT);
    let name = optional_text(operation, "name", take("name"))?;
    let path_value = take("path");
    let paths_value = take(PATHS_KEY);
    if paths_value.is_unit() && name.is_none() {
        return Err(mismatch(
            "the key name (the rule's name)".into(),
            "no name".into(),
        ));
    }
    let label = optional_text(operation, "label", take("label"))?.ok_or_else(|| {
        mismatch(
            "the key label (the vertex or edge label)".into(),
            "no label".into(),
        )
    })?;
    // `paths`: one rule per path, each under its generated name.
    let paths: Vec<crate::value::JsonPath> = if !paths_value.is_unit() {
        if !path_value.is_unit() || name.is_some() {
            return Err(mismatch(
                "either name and path (one rule) or paths without a name (one rule per path, \
                 named <label>_<path>)"
                    .into(),
                if name.is_some() {
                    "name with paths".into()
                } else {
                    "path with paths".into()
                },
            ));
        }
        let list_type = type_name_of(&paths_value);
        let Some(list) = paths_value.try_cast::<rhai::Array>() else {
            return Err(mismatch(
                "paths as a list of paths [\"kind\", \"details.cv\"]".into(),
                list_type,
            ));
        };
        if list.is_empty() {
            return Err(mismatch(
                "paths as a list of at least one path".into(),
                "an empty list".into(),
            ));
        }
        let mut paths = Vec::with_capacity(list.len());
        for item in &list {
            match path_of(item) {
                Some(path) => paths.push(path?),
                None => {
                    return Err(mismatch(
                        "each path as a string (\"details.career.cv\") or a jpath(..)".into(),
                        type_name_of(item),
                    ));
                }
            }
        }
        paths
    } else {
        match path_of(&path_value) {
            Some(path) => vec![path?],
            None => {
                return Err(mismatch(
                    "the key path as a string (\"details.career.cv\") or a jpath(..)".into(),
                    if path_value.is_unit() {
                        "no path".to_string()
                    } else {
                        type_name_of(&path_value)
                    },
                ));
            }
        }
    };
    let element = match optional_text(operation, "element", take("element"))?.as_deref() {
        None | Some("vertex") => ElementKind::Vertex,
        Some("edge") => ElementKind::Edge,
        Some(other) => {
            return Err(mismatch(
                "element \"vertex\" or \"edge\"".into(),
                format!("\"{other}\""),
            ));
        }
    };
    let codec = match optional_text(operation, "codec", take("codec"))? {
        Some(codec) => Some(Codec::from_name(&codec).ok_or_else(|| {
            mismatch("codec \"lz4\" or \"dedup\"".into(), format!("\"{codec}\""))
        })?),
        None => None,
    };
    let min_bytes = take("min_bytes");
    let min_bytes = if min_bytes.is_unit() {
        None
    } else {
        Some(
            min_bytes
                .as_int()
                .ok()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| {
                    mismatch(
                        "min_bytes as an integer from 1 to 16777216".into(),
                        dynamic_text(&min_bytes),
                    )
                })?,
        )
    };
    let dictionary = take("dictionary");
    let dictionary =
        if dictionary.is_unit() {
            None
        } else {
            Some(dictionary.as_bool().map_err(|_| {
                mismatch("dictionary as a boolean".into(), type_name_of(&dictionary))
            })?)
        };
    let mut definitions = Vec::with_capacity(paths.len());
    for path in paths {
        let rule_name = match &name {
            Some(name) => name.clone(),
            None => generated_name(element, &label, &path),
        };
        let mut rule = CompressionDefinition::with_path(element, label.as_str(), path);
        if let Some(codec) = codec {
            rule = rule.with_codec(codec);
        }
        if let Some(min_bytes) = min_bytes {
            rule = rule.with_min_bytes(min_bytes);
        }
        if let Some(dictionary) = dictionary {
            rule = rule.with_dictionary(dictionary);
        }
        let definition = Definition::compression(rule_name, rule);
        definition
            .validate()
            .map_err(|err| graph_fail(crate::error::GraphError::from(err)))?;
        definitions.push(definition);
    }
    Ok(definitions)
}

impl ScriptGraphTraversalSource {
    /// Asks the session's authorizer for `action` on the compression rule `name` (`None`:
    /// every rule).
    fn require_compression(
        &self,
        operation: &str,
        action: Action<'_>,
        name: Option<&str>,
    ) -> Result<(), Box<EvalAltResult>> {
        self.require(
            operation,
            AccessRequest::new(
                action,
                Resource::definition(Some(DefinitionKind::COMPRESSION), name),
            ),
        )
    }

    fn compression_graph(&self, operation: &str) -> Result<&Arc<dyn AnyGraph>, Box<EvalAltResult>> {
        self.g.as_ref().ok_or_else(|| {
            Box::new(EvalAltResult::ErrorRuntime(
                Dynamic::from(format!("{operation}: no graph associated with this source")),
                rhai::Position::NONE,
            ))
        })
    }

    /// `g.define_compression(#{name, label, path, element?, codec?, min_bytes?, dictionary?})`
    /// / `g.define_compression(..)`: stores (or replaces) a compression rule. `path` is a string
    /// read as a jpath (`"details.career.cv"`, because it IS a path) or a `jpath(..)`;
    /// `element` is `"vertex"` (default) or `"edge"`, `codec` `"lz4"` (default: `min_bytes`
    /// 256, `dictionary` true) or `"dedup"` (each distinct value stored once: `min_bytes` 0,
    /// no dictionary; `dictionary: true` is refused). The explicit pass re-encodes the label's
    /// values in the same unit. Asks for `Create` on the rule (`Update` when it replaces one).
    pub fn define_compression(&mut self, spec: Dynamic) -> Result<(), Box<EvalAltResult>> {
        let operation = "define_compression";
        let definitions = rule_definitions(operation, spec, false)?;
        self.store_rules(operation, definitions)
    }

    /// `g.define_compressions([#{..}, ..])`: stores (or replaces) every rule of the list in ONE
    /// unit (one commit) and re-encodes the values of all their labels in ONE walk (book page
    /// "Compressed Properties", section "Several rules at once"). Each spec is
    /// `define_compression`'s map, or `#{label, paths: [..], element?, codec?, min_bytes?,
    /// dictionary?}` for one rule per path, each named `<label>_<path>` (`_edge` added for an
    /// edge; characters a name may not hold become `_`). All or nothing: a refused rule fails
    /// the list with its error and nothing is stored. Asks for `Create` (`Update`) on each
    /// rule before anything changes. Answers the number of rules stored.
    pub fn define_compressions(&mut self, specs: Dynamic) -> Result<i64, Box<EvalAltResult>> {
        let operation = "define_compressions";
        let actual = type_name_of(&specs);
        let Some(specs) = specs.try_cast::<rhai::Array>() else {
            return Err(fail(ScriptError::ArgumentMismatch {
                step_name: operation.to_string(),
                expected: "a list of rule maps [#{name, label, path, ..}, #{label, paths: [..], \
                           ..}]"
                    .into(),
                actual,
            }));
        };
        let mut definitions = Vec::new();
        for spec in specs {
            definitions.extend(rule_definitions(operation, spec, true)?);
        }
        let count = definitions.len() as i64;
        self.store_rules(operation, definitions)?;
        Ok(count)
    }

    /// Asks for `Create` (`Update` when one of that name is stored or comes earlier in the
    /// list) on each rule of `definitions`, then stores them all in one unit
    /// ([`AnyGraph::set_definitions`]: one walk on a `TraversalGraph`).
    fn store_rules(
        &self,
        operation: &str,
        definitions: Vec<Definition>,
    ) -> Result<(), Box<EvalAltResult>> {
        let graph = self.compression_graph(operation)?;
        for (at, definition) in definitions.iter().enumerate() {
            let name = definition.name();
            let earlier = definitions[..at].iter().any(|d| d.name() == name);
            let exists = earlier
                || graph
                    .definition(DefinitionKind::COMPRESSION, name)
                    .map_err(graph_fail)?
                    .is_some();
            let action = if exists {
                Action::Update
            } else {
                Action::Create
            };
            self.require_compression(operation, action, Some(name))?;
        }
        graph.set_definitions(definitions).map_err(script_error)
    }

    /// `g.estimate_compression(#{label, path, element?, codec?, min_bytes?, dictionary?})` /
    /// `g.estimate_compression(..)`: what compression rules for `(element, label, path)` would
    /// hold over the current values, without changing anything (read-only, commits nothing).
    /// Without `codec`/`min_bytes`/`dictionary` every default candidate is measured (`lz4`
    /// with and without a dictionary at 64, 128, 256, 512 and 1024 bytes, plus the current
    /// rule's parameters); each given key narrows the set. `codec` is `"lz4"`, `"dedup"` (at
    /// 0, 25, 64, 128 and 256 bytes) or a list of them (`["lz4", "dedup"]` compares both). Answers
    /// a map
    /// (`estimate_entry`). Asks for `Read` on the data of that element kind and label.
    pub fn estimate_compression(&mut self, spec: Dynamic) -> Result<rhai::Map, Box<EvalAltResult>> {
        let operation = "estimate_compression";
        let actual = type_name_of(&spec);
        let mismatch = |expected: String, actual: String| {
            fail(ScriptError::ArgumentMismatch {
                step_name: operation.to_string(),
                expected,
                actual,
            })
        };
        let Some(mut spec) = spec.try_cast::<rhai::Map>() else {
            return Err(mismatch(
                "a map #{label, path, element?, codec?, min_bytes?, dictionary?}".into(),
                actual,
            ));
        };
        if let Some(unknown) = spec
            .keys()
            .find(|key| !ESTIMATE_KEYS.contains(&key.as_str()))
        {
            return Err(mismatch(
                format!("only the keys {}", ESTIMATE_KEYS.join(", ")),
                format!("the key '{unknown}'"),
            ));
        }
        let mut take = |key: &str| spec.remove(key).unwrap_or(Dynamic::UNIT);
        let label = optional_text(operation, "label", take("label"))?.ok_or_else(|| {
            mismatch(
                "the key label (the vertex or edge label)".into(),
                "no label".into(),
            )
        })?;
        let path_value = take("path");
        let path: crate::value::JsonPath =
            if let Some(path) = path_value.read_lock::<crate::value::JsonPath>() {
                path.clone()
            } else if path_value.is_string() {
                path_value
                    .to_string()
                    .parse()
                    .map_err(|err: crate::TraverserError| script_error(Box::new(err)))?
            } else {
                return Err(mismatch(
                    "the key path as a string (\"details.career.cv\") or a jpath(..)".into(),
                    if path_value.is_unit() {
                        "no path".to_string()
                    } else {
                        type_name_of(&path_value)
                    },
                ));
            };
        let element = match optional_text(operation, "element", take("element"))?.as_deref() {
            None | Some("vertex") => ElementKind::Vertex,
            Some("edge") => ElementKind::Edge,
            Some(other) => {
                return Err(mismatch(
                    "element \"vertex\" or \"edge\"".into(),
                    format!("\"{other}\""),
                ));
            }
        };
        let codec_value = take("codec");
        let codec_names: Vec<String> = if codec_value.is_unit() {
            Vec::new()
        } else if let Some(list) = codec_value.read_lock::<rhai::Array>() {
            let mut names = Vec::new();
            for item in list.iter() {
                match item.clone().into_immutable_string() {
                    Ok(name) => names.push(name.to_string()),
                    Err(actual) => {
                        return Err(mismatch(
                            "codec as \"lz4\", \"dedup\" or a list of them".into(),
                            format!("a list holding a {actual}"),
                        ));
                    }
                }
            }
            names
        } else {
            match optional_text(operation, "codec", codec_value)? {
                Some(name) => vec![name],
                None => Vec::new(),
            }
        };
        let mut codecs: Vec<Codec> = Vec::new();
        for name in &codec_names {
            let codec = Codec::from_name(name).ok_or_else(|| {
                mismatch(
                    "codec \"lz4\", \"dedup\" or a list of them".into(),
                    format!("\"{name}\""),
                )
            })?;
            codecs.push(codec);
        }
        let min_bytes = take("min_bytes");
        let min_bytes = if min_bytes.is_unit() {
            None
        } else {
            Some(
                min_bytes
                    .as_int()
                    .ok()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| {
                        mismatch(
                            "min_bytes as an integer from 1 to 16777216".into(),
                            dynamic_text(&min_bytes),
                        )
                    })?,
            )
        };
        let dictionary = take("dictionary");
        let dictionary = if dictionary.is_unit() {
            None
        } else {
            Some(dictionary.as_bool().map_err(|_| {
                mismatch("dictionary as a boolean".into(), type_name_of(&dictionary))
            })?)
        };
        let request = CompressionEstimateRequest::with_path(element, label.as_str(), path)
            .with_candidates(CompressionCandidate::matching_codecs(
                &codecs, min_bytes, dictionary,
            ));
        request
            .validate()
            .map_err(|err| graph_fail(crate::error::GraphError::from(err)))?;
        self.require(
            operation,
            AccessRequest::new(
                Action::Read,
                Resource::data(Some(element), Some(label.as_str())),
            ),
        )?;
        let estimate = self
            .compression_graph(operation)?
            .estimate_compression(&request)
            .map_err(script_error)?;
        Ok(estimate_entry(&estimate))
    }

    /// `g.compressions()`: every compression rule as a map (name, element, label, path, codec,
    /// min_bytes, dictionary, compressed_values, plain_bytes, stored_bytes, dictionary_bytes,
    /// distinct_values), by name. Asks for `Read` on the rules.
    pub fn compressions(&mut self) -> Result<rhai::Array, Box<EvalAltResult>> {
        self.require_compression("compressions", Action::Read, None)?;
        let graph = self.compression_graph("compressions")?;
        let mut entries = rhai::Array::new();
        for definition in graph
            .definitions(Some(DefinitionKind::COMPRESSION))
            .map_err(graph_fail)?
        {
            let stats = graph
                .compression_stats(definition.name())
                .map_err(graph_fail)?;
            if let Some(entry) = rule_entry(&definition, stats) {
                entries.push(Dynamic::from_map(entry));
            }
        }
        Ok(entries)
    }

    /// `g.drop_compression(name)`: removes the rule and decompresses
    /// its values (in the same unit); `false` when there is none. Asks for `Delete` on it.
    pub fn drop_compression(&mut self, name: &str) -> Result<bool, Box<EvalAltResult>> {
        self.require_compression("drop_compression", Action::Delete, Some(name))?;
        self.compression_graph("drop_compression")?
            .remove_definition(DefinitionKind::COMPRESSION, name)
            .map_err(script_error)
    }

    /// `g.drop_compressions([name, ..])`: removes every rule of the list in ONE unit (one commit)
    /// and decompresses the values of all their labels in ONE walk (book page "Compressed
    /// Properties", section "Several rules at once"). A name that is not stored is skipped. Asks
    /// for `Delete` on each rule before anything changes. Answers the number of rules removed.
    pub fn drop_compressions(&mut self, names: Dynamic) -> Result<i64, Box<EvalAltResult>> {
        let operation = "drop_compressions";
        let actual = type_name_of(&names);
        let mismatch = |actual: String| {
            fail(ScriptError::ArgumentMismatch {
                step_name: operation.to_string(),
                expected: "a list of rule names [\"a\", \"b\"]".into(),
                actual,
            })
        };
        let Some(items) = names.try_cast::<rhai::Array>() else {
            return Err(mismatch(actual));
        };
        let mut names: Vec<String> = Vec::with_capacity(items.len());
        for item in items {
            let actual = type_name_of(&item);
            names.push(item.into_string().map_err(|_| mismatch(actual))?);
        }
        for name in &names {
            self.require_compression(operation, Action::Delete, Some(name))?;
        }
        let definitions: Vec<(DefinitionKind, &str)> = names
            .iter()
            .map(|name| (DefinitionKind::COMPRESSION, name.as_str()))
            .collect();
        let removed = self
            .compression_graph(operation)?
            .remove_definitions(&definitions)
            .map_err(script_error)?;
        Ok(removed as i64)
    }

    /// `g.recompress(name)`: rebuilds the rule's dictionary from the current values and
    /// re-encodes them; changes no logical data (commits nothing). Fails when there is no such
    /// rule. Asks for `Update` on it.
    pub fn recompress(&mut self, name: &str) -> Result<(), Box<EvalAltResult>> {
        self.require_compression("recompress", Action::Update, Some(name))?;
        self.compression_graph("recompress")?
            .recompress(name)
            .map_err(script_error)
    }
}
