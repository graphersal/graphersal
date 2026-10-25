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
    /// 256, `dictionary` true) or `"dedup"` (each distinct value stored once: `min_bytes` 25,
    /// no dictionary; `dictionary: true` is refused). The explicit pass re-encodes the label's
    /// values in the same unit. Asks for `Create` on the rule (`Update` when it replaces one).
    pub fn define_compression(&mut self, spec: Dynamic) -> Result<(), Box<EvalAltResult>> {
        let operation = "define_compression";
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
                "a map #{name, label, path, element?, codec?, min_bytes?, dictionary?}".into(),
                actual,
            ));
        };
        if let Some(unknown) = spec.keys().find(|key| !KEYS.contains(&key.as_str())) {
            return Err(mismatch(
                format!("only the keys {}", KEYS.join(", ")),
                format!("the key '{unknown}'"),
            ));
        }
        let mut take = |key: &str| spec.remove(key).unwrap_or(Dynamic::UNIT);
        let name = optional_text(operation, "name", take("name"))?
            .ok_or_else(|| mismatch("the key name (the rule's name)".into(), "no name".into()))?;
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
        let mut rule = CompressionDefinition::with_path(element, label, path);
        if let Some(codec) = optional_text(operation, "codec", take("codec"))? {
            let codec = Codec::from_name(&codec).ok_or_else(|| {
                mismatch("codec \"lz4\" or \"dedup\"".into(), format!("\"{codec}\""))
            })?;
            rule = rule.with_codec(codec);
        }
        let min_bytes = take("min_bytes");
        if !min_bytes.is_unit() {
            let value = min_bytes
                .as_int()
                .ok()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| {
                    mismatch(
                        "min_bytes as an integer from 1 to 16777216".into(),
                        dynamic_text(&min_bytes),
                    )
                })?;
            rule = rule.with_min_bytes(value);
        }
        let dictionary = take("dictionary");
        if !dictionary.is_unit() {
            let value = dictionary.as_bool().map_err(|_| {
                mismatch("dictionary as a boolean".into(), type_name_of(&dictionary))
            })?;
            rule = rule.with_dictionary(value);
        }
        let definition = Definition::compression(name.as_str(), rule);
        definition
            .validate()
            .map_err(|err| graph_fail(crate::error::GraphError::from(err)))?;
        let graph = self.compression_graph(operation)?;
        let exists = graph
            .definition(DefinitionKind::COMPRESSION, &name)
            .is_some();
        let action = if exists {
            Action::Update
        } else {
            Action::Create
        };
        self.require_compression(operation, action, Some(&name))?;
        graph
            .set_definitions(vec![definition])
            .map_err(script_error)
    }

    /// `g.estimate_compression(#{label, path, element?, codec?, min_bytes?, dictionary?})` /
    /// `g.estimate_compression(..)`: what compression rules for `(element, label, path)` would
    /// hold over the current values, without changing anything (read-only, commits nothing).
    /// Without `codec`/`min_bytes`/`dictionary` every default candidate is measured (`lz4`
    /// with and without a dictionary at 64, 128, 256, 512 and 1024 bytes, plus the current
    /// rule's parameters); each given key narrows the set. `codec` is `"lz4"`, `"dedup"` (at
    /// 25, 64, 128 and 256 bytes) or a list of them (`["lz4", "dedup"]` compares both). Answers
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
        Ok(graph
            .definitions(Some(DefinitionKind::COMPRESSION))
            .iter()
            .filter_map(|definition| {
                rule_entry(definition, graph.compression_stats(definition.name()))
            })
            .map(Dynamic::from_map)
            .collect())
    }

    /// `g.drop_compression(name)`: removes the rule and decompresses
    /// its values (in the same unit); `false` when there is none. Asks for `Delete` on it.
    pub fn drop_compression(&mut self, name: &str) -> Result<bool, Box<EvalAltResult>> {
        self.require_compression("drop_compression", Action::Delete, Some(name))?;
        self.compression_graph("drop_compression")?
            .remove_definition(DefinitionKind::COMPRESSION, name)
            .map_err(script_error)
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
