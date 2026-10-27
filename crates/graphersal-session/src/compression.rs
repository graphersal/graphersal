//! Compression rules in the session (`book/src/users_guide/compressed_properties.md`; book page "Compressed
//! Properties"): the operations behind the playground's Catalog ▸ Compression manager, the dev
//! server's `/api/compression*` endpoints and its MCP tools `list_compressions`,
//! `define_compression`, `drop_compression`, `recompress`.
//!
//! Every operation works on the catalog of the graph the session serves (a Store's graph too:
//! a change is a commit like any other, journaled and recorded in the dev server's change
//! feed; a `recompress` changes no data and commits nothing).
//!
//! JSON forms:
//!
//! - an **entry** (`list_compressions`): `{name, element: "vertex"|"edge", label, path, codec:
//!   "lz4"|"dedup", minBytes, dictionary, served, reason, stats: {compressedValues,
//!   plainBytes, storedBytes, dictionaryBytes, distinctValues}}`; `path` is the canonical
//!   jpath text (`$.details.career.cv`); for a `dedup` rule `compressedValues` counts the
//!   shared values and `distinctValues` the distinct texts among them; `served` is false and
//!   `stats` `null` when the rule is not applied, and `reason` (else `null`) says why: the
//!   storage applies no compression rules, or the rule's `codec`, `element` or `path` is not
//!   known to this version (a newer one wrote it, kept as read,
//!   [`CompressionDefinition::unserved_reason`]; `codec`/`element`/`path` show the names as
//!   read);
//! - a **spec** (`define_compression`): `{name, label, path, element?, codec?, minBytes?,
//!   dictionary?, replaces?}`; `codec` is `"lz4"` (default: minBytes 256, a dictionary) or
//!   `"dedup"` (minBytes 25, no dictionary: `dictionary: true` is a field error); `replaces`
//!   names the rule an edit started from (a rename stores the new name and drops the old one
//!   in one commit);
//! - **field errors** (a refused define): `[{field, message, help?}]` with `field` one of
//!   `name`, `element`, `label`, `path`, `codec`, `minBytes`, `dictionary` or `general`, the
//!   same shape as the saved queries' editor.
//! - an **estimate spec** (`estimate_compression`, read-only): `{label, path, element?, codec?,
//!   minBytes?, dictionary?}`; each optional key given narrows the candidates (none: `lz4`
//!   with and without a dictionary at 64, 128, 256, 512 and 1024 bytes, plus the current
//!   rule's parameters); `codec` is a name or a list of names (`["lz4", "dedup"]` measures
//!   both; `dedup` at 25, 64, 128 and 256 bytes);
//! - an **estimate** (`{ok: true, estimate}`): `{element, label, path, elements, valuesFound,
//!   stringValues, stringBytes, sampled, measuredValues, histogram: [{minBytes, maxBytes,
//!   values, bytes}], candidates: [{codec, minBytes, dictionary, compressedValues, plainBytes,
//!   storedBytes, dictionaryBytes, distinctValues, savedBytes, ratio, current,
//!   encodeBytesPerSec, decodeBytesPerSec}], best, current: {name, codec, minBytes, dictionary,
//!   stats} | null,
//!   warnings: [{code, message}], elapsedMs, graphChanged: false}`; `best` is the index of the
//!   candidate that saves the most (`null`: none saves anything).

use crate::error::SessionError;
use graphersal::catalog::{
    Codec, CompressionCandidate, CompressionDefinition, CompressionEstimate,
    CompressionEstimateRequest,
};
use graphersal::catalog::{Definition, DefinitionKind};
use graphersal::prelude::*;
use graphersal::{catalog::CompressionStats, storage::ElementKind};
use serde_json::{Value, json};

use crate::Session;
use crate::queries::FieldError;

/// The keys a spec may have.
const KEYS: [&str; 8] = [
    "name",
    "label",
    "path",
    "element",
    "codec",
    "minBytes",
    "dictionary",
    "replaces",
];

/// The entry of one rule (module docs); `stats` is `null` when the storage reports none (a
/// storage without compressed properties keeps the rule but never applies it).
/// Why a rule of a known codec is not applied: the storage applies no compression rules.
const STORAGE_APPLIES_NONE: &str = "this storage keeps the rule in its catalog but does not \
                                    apply compression rules; values stay plain";

fn entry(name: &str, rule: &CompressionDefinition, stats: Option<CompressionStats>) -> Value {
    let stats = stats.map(|stats| {
        json!({
            "compressedValues": stats.compressed_values,
            "plainBytes": stats.plain_bytes,
            "storedBytes": stats.stored_bytes,
            "dictionaryBytes": stats.dictionary_bytes,
            "distinctValues": stats.distinct_values,
        })
    });
    let served = stats.is_some();
    let reason = (!served).then(|| {
        rule.unserved_reason()
            .unwrap_or_else(|| STORAGE_APPLIES_NONE.to_string())
    });
    json!({
        "name": name,
        "element": rule.element_name(),
        "label": rule.label(),
        "path": rule.path_text(),
        "codec": rule.codec_name(),
        "minBytes": rule.min_bytes(),
        "dictionary": rule.dictionary(),
        "served": served,
        "reason": reason,
        "stats": stats,
    })
}

/// The keys an estimate spec may have.
const ESTIMATE_KEYS: [&str; 6] = [
    "label",
    "path",
    "element",
    "codec",
    "minBytes",
    "dictionary",
];

/// An estimate as JSON (module docs).
pub fn estimate_json(estimate: &CompressionEstimate) -> Value {
    let candidates: Vec<Value> = estimate
        .candidates
        .iter()
        .map(|c| {
            json!({
                "codec": c.candidate.codec.name(),
                "minBytes": c.candidate.min_bytes,
                "dictionary": c.candidate.dictionary,
                "compressedValues": c.compressed_values,
                "plainBytes": c.plain_bytes,
                "storedBytes": c.stored_bytes,
                "dictionaryBytes": c.dictionary_bytes,
                "distinctValues": c.distinct_values,
                "savedBytes": c.saved_bytes,
                "ratio": c.ratio(),
                "current": c.current,
                "encodeBytesPerSec": c.encode_bytes_per_sec,
                "decodeBytesPerSec": c.decode_bytes_per_sec,
            })
        })
        .collect();
    let histogram: Vec<Value> = estimate
        .histogram
        .iter()
        .map(|b| {
            json!({
                "minBytes": b.min_bytes,
                "maxBytes": b.max_bytes,
                "values": b.values,
                "bytes": b.bytes,
            })
        })
        .collect();
    let current = estimate.current.as_ref().map(|current| {
        json!({
            "name": current.name,
            "codec": current.rule.codec().name(),
            "minBytes": current.rule.min_bytes(),
            "dictionary": current.rule.dictionary(),
            "stats": {
                "compressedValues": current.stats.compressed_values,
                "plainBytes": current.stats.plain_bytes,
                "storedBytes": current.stats.stored_bytes,
                "dictionaryBytes": current.stats.dictionary_bytes,
                "distinctValues": current.stats.distinct_values,
            },
        })
    });
    let warnings: Vec<Value> = estimate
        .warnings
        .iter()
        .map(|w| json!({"code": w.code(), "message": w.message(estimate)}))
        .collect();
    json!({
        "element": estimate.element.as_str(),
        "label": estimate.label,
        "path": estimate.path,
        "elements": estimate.elements,
        "valuesFound": estimate.values_found,
        "stringValues": estimate.string_values,
        "stringBytes": estimate.string_bytes,
        "sampled": estimate.sampled,
        "measuredValues": estimate.measured_values,
        "histogram": histogram,
        "candidates": candidates,
        "best": estimate.best,
        "current": current,
        "warnings": warnings,
        "elapsedMs": estimate.elapsed_micros as f64 / 1000.0,
        "graphChanged": false,
    })
}

/// The request of an estimate spec (module docs), or the field errors of the form.
pub fn estimate_request(spec: &Value) -> Result<CompressionEstimateRequest, Vec<FieldError>> {
    let Some(object) = spec.as_object() else {
        return Err(vec![FieldError::new(
            "general",
            "the estimate is a JSON object {label, path, element?, codec?, minBytes?, \
             dictionary?}",
        )]);
    };
    if let Some(unknown) = object
        .keys()
        .find(|key| !ESTIMATE_KEYS.contains(&key.as_str()))
    {
        return Err(vec![FieldError::new(
            "general",
            format!(
                "unknown key '{unknown}' (the keys are {})",
                ESTIMATE_KEYS.join(", ")
            ),
        )]);
    }
    let mut errors = Vec::new();
    let mut text = |key: &str| match spec.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) if text.trim().is_empty() => None,
        Some(Value::String(text)) => Some(text.trim().to_string()),
        Some(_) => {
            errors.push(FieldError::new(key, format!("{key} must be a string")));
            None
        }
    };
    let label = text("label").unwrap_or_default();
    let path = text("path").unwrap_or_default();
    let element = text("element");
    // `codec` is a name or a list of names.
    let codec_names: Vec<String> = match spec.get("codec") {
        Some(Value::Array(items)) => {
            let names: Vec<String> = items
                .iter()
                .filter_map(|item| item.as_str().map(|name| name.trim().to_string()))
                .collect();
            if names.len() != items.len() {
                errors.push(FieldError::new(
                    "codec",
                    "codec is a codec name or a list of them (\"lz4\", \"dedup\")",
                ));
            }
            names
        }
        _ => text("codec").into_iter().collect(),
    };
    if label.is_empty() {
        errors.push(FieldError::new(
            "label",
            "name the vertex or edge label to estimate",
        ));
    }
    if path.is_empty() {
        errors.push(FieldError::new(
            "path",
            "name the property path, for example cv or details.career.cv",
        ));
    }
    let element = match element.as_deref() {
        None | Some("vertex") => ElementKind::Vertex,
        Some("edge") => ElementKind::Edge,
        Some(other) => {
            errors.push(FieldError::new(
                "element",
                format!("'{other}' is not vertex or edge"),
            ));
            ElementKind::Vertex
        }
    };
    let mut codecs: Vec<Codec> = Vec::new();
    for name in &codec_names {
        match Codec::from_name(name) {
            Some(codec) => codecs.push(codec),
            None => errors.push(FieldError::new(
                "codec",
                format!("unknown codec '{name}' (this version has lz4 and dedup)"),
            )),
        }
    }
    let min_bytes = match spec.get("minBytes") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_u64().and_then(|n| u32::try_from(n).ok()) {
            Some(n) => Some(n),
            None => {
                errors.push(FieldError::new(
                    "minBytes",
                    "minBytes is a whole number of bytes (1 to 16777216)",
                ));
                None
            }
        },
    };
    let dictionary = match spec.get("dictionary") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(flag)) => Some(*flag),
        Some(_) => {
            errors.push(FieldError::new("dictionary", "dictionary is true or false"));
            None
        }
    };
    let mut parsed = None;
    if !path.is_empty() {
        match path.parse::<JsonPath>() {
            Ok(path) => parsed = Some(path),
            Err(err) => errors.push(FieldError::new("path", err.to_string())),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let Some(path) = parsed else {
        return Err(vec![FieldError::new("path", "the path is missing")]);
    };
    let request = CompressionEstimateRequest::with_path(element, label, path).with_candidates(
        CompressionCandidate::matching_codecs(&codecs, min_bytes, dictionary),
    );
    request.validate().map_err(|err| vec![field_of(&err)])?;
    Ok(request)
}

/// The editor field a catalog error of a compression rule belongs to.
fn field_of(err: &DefinitionError) -> FieldError {
    let mut error = FieldError::of(err);
    error.field = match err {
        DefinitionError::InvalidName { .. } => "name".to_string(),
        DefinitionError::InvalidField { field, .. } => match field.as_str() {
            "label" | "path" | "dictionary" | "codec" => field.clone(),
            "min_bytes" => "minBytes".to_string(),
            _ => "general".to_string(),
        },
        DefinitionError::Conflict { .. } => error.field.clone(),
        _ => "general".to_string(),
    };
    error
}

impl Session {
    /// The compression rules of the graph, by name, with their current stats (module docs).
    /// Fails when the graph cannot be read ([`SessionError::Graph`]).
    pub fn list_compressions(&self) -> Result<Value, SessionError> {
        Ok(self.graph.compressions()?)
    }

    /// The rule `name` as its entry, `null` when there is none. Fails like
    /// [`Self::list_compressions`].
    pub fn get_compression(&self, name: &str) -> Result<Value, SessionError> {
        Ok(self.graph.compression(name)?)
    }

    /// How many compression rules the graph holds. Fails like [`Self::list_compressions`].
    pub fn compression_count(&self) -> Result<usize, SessionError> {
        Ok(self.graph.count_kind(DefinitionKind::COMPRESSION)?)
    }

    /// Defines (or replaces) a compression rule from a spec (module docs), in ONE commit: the
    /// explicit pass re-encodes the label's values in it. Without `replaces` an existing rule
    /// of that name is not overwritten (the form's "New"); with `replaces` naming another
    /// rule, the new name must be free and the old rule is dropped in the same commit (a
    /// rename). Answers `{ok: true, rule: <entry>}` or `{ok: false, errors: [{field, message,
    /// help?}]}`, never fails otherwise.
    pub fn define_compression(&mut self, spec: &Value) -> Value {
        match self.define_compression_checked(spec) {
            Ok(entry) => json!({"ok": true, "rule": entry}),
            Err(errors) => json!({
                "ok": false,
                "errors": errors.iter().map(FieldError::to_value).collect::<Vec<_>>(),
            }),
        }
    }

    fn define_compression_checked(&mut self, spec: &Value) -> Result<Value, Vec<FieldError>> {
        let Some(object) = spec.as_object() else {
            return Err(vec![FieldError::new(
                "general",
                "the rule is a JSON object {name, label, path, element?, codec?, minBytes?, \
                 dictionary?, replaces?}",
            )]);
        };
        if let Some(unknown) = object.keys().find(|key| !KEYS.contains(&key.as_str())) {
            return Err(vec![FieldError::new(
                "general",
                format!("unknown key '{unknown}' (the keys are {})", KEYS.join(", ")),
            )]);
        }
        let mut errors = Vec::new();
        let mut text = |key: &str| match spec.get(key) {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) if text.trim().is_empty() => None,
            Some(Value::String(text)) => Some(text.trim().to_string()),
            Some(_) => {
                errors.push(FieldError::new(key, format!("{key} must be a string")));
                None
            }
        };
        let name = text("name").unwrap_or_default();
        let label = text("label").unwrap_or_default();
        let path = text("path").unwrap_or_default();
        let element = text("element");
        let codec = text("codec");
        let replaces = text("replaces").filter(|old| *old != name);
        let is_edit = spec
            .get("replaces")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        if name.is_empty() {
            errors.push(FieldError::new(
                "name",
                "a compression rule needs a name (an identifier such as customer_cv)",
            ));
        }
        if label.is_empty() {
            errors.push(FieldError::new(
                "label",
                "name the vertex or edge label the rule applies to",
            ));
        }
        if path.is_empty() {
            errors.push(FieldError::new(
                "path",
                "name the property path, for example cv or details.career.cv",
            ));
        }
        let element = match element.as_deref() {
            None | Some("vertex") => ElementKind::Vertex,
            Some("edge") => ElementKind::Edge,
            Some(other) => {
                errors.push(FieldError::new(
                    "element",
                    format!("'{other}' is not vertex or edge"),
                ));
                ElementKind::Vertex
            }
        };
        let codec = match codec.as_deref() {
            None => Codec::Lz4,
            Some(name) => Codec::from_name(name).unwrap_or_else(|| {
                errors.push(FieldError::new(
                    "codec",
                    format!("unknown codec '{name}' (this version has lz4 and dedup)"),
                ));
                Codec::Lz4
            }),
        };
        let min_bytes = match spec.get("minBytes") {
            None | Some(Value::Null) => None,
            Some(value) => match value.as_u64().and_then(|n| u32::try_from(n).ok()) {
                Some(n) => Some(n),
                None => {
                    errors.push(FieldError::new(
                        "minBytes",
                        "minBytes is a whole number of bytes (1 to 16777216)",
                    ));
                    None
                }
            },
        };
        let dictionary = match spec.get("dictionary") {
            None | Some(Value::Null) => None,
            Some(Value::Bool(flag)) => Some(*flag),
            Some(_) => {
                errors.push(FieldError::new("dictionary", "dictionary is true or false"));
                None
            }
        };
        let mut rule = None;
        if !path.is_empty() {
            match path.parse::<JsonPath>() {
                Ok(parsed) => {
                    rule = Some(CompressionDefinition::with_path(element, &*label, parsed))
                }
                Err(err) => errors.push(FieldError::new("path", err.to_string())),
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let Some(mut rule) = rule else {
            return Err(vec![FieldError::new("path", "the path is missing")]);
        };
        rule = rule.with_codec(codec);
        if let Some(min_bytes) = min_bytes {
            rule = rule.with_min_bytes(min_bytes);
        }
        if let Some(dictionary) = dictionary {
            rule = rule.with_dictionary(dictionary);
        }
        // The name and the rule each on their own, so both wrong fields are reported.
        let mut errors = Vec::new();
        if let Err(err) = Definition::compression(
            name.as_str(),
            CompressionDefinition::with_path(
                element,
                "probe",
                "probe".parse().map_err(|err: graphersal::TraverserError| {
                    vec![FieldError::new("general", err.to_string())]
                })?,
            ),
        )
        .validate()
        {
            errors.push(field_of(&err));
        }
        if let Err(err) = Definition::compression("probe", rule.clone()).validate() {
            errors.push(field_of(&err));
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let definition = Definition::compression(name.as_str(), rule);
        self.graph
            .store_compression(definition, is_edit, replaces.as_deref())?;
        self.get_compression(&name)
            .map_err(|err| vec![FieldError::new("general", err.to_string())])
    }

    /// Estimates compression rules for the `(element, label, path)` of an estimate spec
    /// (module docs) without changing the graph: answers `{ok: true, estimate}` or `{ok:
    /// false, errors: [{field, message, help?}]}` (a storage without compressed properties, a
    /// cancelled estimate: one `general` error), never fails otherwise. Bounded (sampled above
    /// the engine's caps) and cancellable through [`Self::cancel_token`].
    pub fn estimate_compression(&self, spec: &Value) -> Value {
        let answer = estimate_request(spec).and_then(|request| {
            let cancel = self.cancel_token();
            cancel.reset();
            self.graph
                .estimate_compression_json(&request.with_cancel(cancel))
                .map_err(|message| vec![FieldError::new("general", message.to_string())])
        });
        match answer {
            Ok(estimate) => json!({"ok": true, "estimate": estimate}),
            Err(errors) => json!({
                "ok": false,
                "errors": errors.iter().map(FieldError::to_value).collect::<Vec<_>>(),
            }),
        }
    }

    /// Drops the compression rule `name` (its values are decompressed in the same commit):
    /// `true`, or `false` when there was none.
    pub fn drop_compression(&mut self, name: &str) -> Result<bool, SessionError> {
        self.graph
            .drop_definition(DefinitionKind::COMPRESSION, name)
    }

    /// Re-runs the explicit pass of the rule `name` (a new dictionary from the current values,
    /// every value re-encoded); changes no data, commits nothing. Answers the rule's entry.
    pub fn recompress(&mut self, name: &str) -> Result<Value, SessionError> {
        self.graph.recompress_rule(name)?;
        self.get_compression(name)
    }
}

/// The compression rules of `g`, by name, as their entries (module docs); the storage's
/// error when it cannot read its catalog or the rules' stats.
pub fn compressions_in<S: GraphStorage>(g: &S) -> Result<Value, GraphError> {
    let mut entries = Vec::new();
    for definition in g.definitions()? {
        let Some(rule) = definition.as_compression() else {
            continue;
        };
        entries.push(entry(
            definition.name(),
            rule,
            g.compression_stats(definition.name())?,
        ));
    }
    Ok(Value::Array(entries))
}

/// The compression rule `name` of `g` as its entry, `null` when there is none; the storage's
/// error when it cannot read it.
pub fn compression_in<S: GraphStorage>(g: &S, name: &str) -> Result<Value, GraphError> {
    Ok(
        match g
            .definition(DefinitionKind::COMPRESSION, name)?
            .and_then(Definition::as_compression)
        {
            Some(rule) => entry(name, rule, g.compression_stats(name)?),
            None => Value::Null,
        },
    )
}

/// The estimate of `request` over `g` as JSON ([`estimate_json`]); an error (a storage without
/// compressed properties: `Unsupported`; a cancelled estimate) as its diagnostic text.
pub fn estimate_compression_in<S: GraphStorage>(
    g: &S,
    request: &CompressionEstimateRequest,
) -> Result<Value, SessionError> {
    GraphTraversalSource::new(g)
        .estimate_compression(request)
        .map(|estimate| estimate_json(&estimate))
        .map_err(|err| format_diagnostic(&*err))
        .map_err(SessionError::from)
}

/// Stores the checked compression rule `definition` in `g` in ONE commit, with the form's
/// name rules: a new rule (not `is_edit`) or a rename (`replaces`) needs a free name;
/// `replaces` must still exist and is dropped in the same commit.
pub(crate) fn store_compression<S: GraphStorage>(
    g: &mut S,
    definition: Definition,
    is_edit: bool,
    replaces: Option<&str>,
) -> Result<(), Vec<FieldError>> {
    let name = definition.name().to_string();
    let general = |err: GraphError| vec![FieldError::new("general", format_diagnostic(&err))];
    let exists = |g: &S, name: &str| {
        g.definition(DefinitionKind::COMPRESSION, name)
            .map(|found| found.is_some())
            .map_err(general)
    };
    if (!is_edit || replaces.is_some()) && exists(g, &name)? {
        return Err(vec![FieldError::new(
            "name",
            format!("a compression rule named '{name}' exists already; choose another name"),
        )]);
    }
    if let Some(old) = replaces
        && !exists(g, old)?
    {
        return Err(vec![FieldError::new(
            "general",
            format!("the compression rule '{old}' no longer exists (dropped elsewhere?)"),
        )]);
    }
    let stored = g.transaction(|unit| {
        if let Some(old) = replaces {
            unit.remove_definition(DefinitionKind::COMPRESSION, old)?;
        }
        unit.set_definition(definition)?;
        Ok::<(), GraphError>(())
    });
    match stored {
        Ok(()) => Ok(()),
        Err(GraphError::Definition(err)) => Err(vec![field_of(&err)]),
        Err(err) => Err(vec![FieldError::new("general", format_diagnostic(&err))]),
    }
}

/// Re-runs the explicit pass of the rule `name` of `g` (changes no data, commits nothing); an
/// error (no such rule; a storage without compressed properties: `Unsupported`) as its
/// diagnostic text.
pub fn recompress_in<S: GraphStorage>(g: &mut S, name: &str) -> Result<(), SessionError> {
    GraphTraversalSource::new(g)
        .recompress(name)
        .map_err(|err| format_diagnostic(&*err))
        .map_err(SessionError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_with_text() -> Session {
        let mut s = Session::new();
        let cv = "Senior engineer, led the platform team in Rust. ".repeat(20);
        let out = s.execute(
            &format!(r#"g.add_v("customer").property("cv", "{cv}").to_list()"#),
            &crate::RunOptions::default(),
        );
        assert!(out.contains("\"ok\":true"), "{out}");
        s
    }

    #[test]
    fn define_list_recompress_rename_and_drop() {
        let mut s = session_with_text();
        let defined = s.define_compression(&json!({
            "name": "cv", "label": "customer", "path": "cv", "minBytes": 64
        }));
        assert_eq!(defined["ok"], true, "{defined}");
        assert_eq!(defined["rule"]["path"], "$.cv");
        assert_eq!(defined["rule"]["stats"]["compressedValues"], 1);
        assert_eq!(s.list_compressions().unwrap().as_array().unwrap().len(), 1);
        assert_eq!(s.compression_count().unwrap(), 1);
        let again = s.define_compression(&json!({"name": "cv", "label": "customer", "path": "x"}));
        assert_eq!(again["errors"][0]["field"], "name", "{again}");
        let edited = s.define_compression(&json!({
            "name": "cv2", "label": "customer", "path": "cv", "replaces": "cv",
            "dictionary": false
        }));
        assert_eq!(edited["ok"], true, "{edited}");
        assert_eq!(s.list_compressions().unwrap()[0]["name"], "cv2");
        assert_eq!(s.recompress("cv2").unwrap()["stats"]["compressedValues"], 1);
        assert!(s.recompress("nope").is_err());
        assert!(s.drop_compression("cv2").unwrap());
        assert!(!s.drop_compression("cv2").unwrap());
    }

    #[test]
    fn field_errors_name_their_inputs() {
        let mut s = session_with_text();
        let refused = s.define_compression(&json!({
            "name": "bad name", "label": "", "path": "a[0]", "element": "node",
            "minBytes": -1, "codec": "zip", "dictionary": "yes"
        }));
        assert_eq!(refused["ok"], false);
        let fields: Vec<&str> = refused["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["field"].as_str().unwrap())
            .collect();
        for field in ["label", "element", "codec", "minBytes", "dictionary"] {
            assert!(fields.contains(&field), "{field}: {refused}");
        }
        let refused = s.define_compression(&json!({
            "name": "bad name", "label": "customer", "path": "a[0]"
        }));
        assert_eq!(refused["errors"][0]["field"], "name", "{refused}");
        assert_eq!(refused["errors"][1]["field"], "path", "{refused}");
        let refused = s.define_compression(&json!({
            "name": "bad name", "label": "customer", "path": "a"
        }));
        assert_eq!(refused["errors"][0]["field"], "name", "{refused}");
        assert!(refused["errors"][0]["help"].is_string());
        s.define_compression(&json!({"name": "a", "label": "customer", "path": "cv"}));
        let conflict =
            s.define_compression(&json!({"name": "b", "label": "customer", "path": "cv"}));
        assert_eq!(conflict["errors"][0]["field"], "path", "{conflict}");
        assert!(
            conflict["errors"][0]["help"]
                .as_str()
                .unwrap()
                .contains("drop_compression")
        );
    }

    #[test]
    fn an_estimate_answers_candidates_and_changes_nothing() {
        let mut s = session_with_text();
        let info = s.statistics().unwrap();
        let answer = s.estimate_compression(&json!({"label": "customer", "path": "cv"}));
        assert_eq!(answer["ok"], true, "{answer}");
        let estimate = &answer["estimate"];
        assert_eq!(estimate["path"], "$.cv");
        assert_eq!(estimate["valuesFound"], 1);
        assert_eq!(estimate["graphChanged"], false);
        assert_eq!(estimate["candidates"].as_array().unwrap().len(), 10);
        assert_eq!(estimate["histogram"][0]["minBytes"], 0);
        assert!(estimate["best"].is_u64(), "{estimate}");
        assert_eq!(estimate["current"], Value::Null);
        assert_eq!(s.statistics().unwrap(), info, "nothing changed");
        assert_eq!(s.compression_count().unwrap(), 0);
        // One candidate; defining it gives what it promised.
        let one = s.estimate_compression(&json!({
            "label": "customer", "path": "cv", "minBytes": 64, "dictionary": false
        }));
        let candidates = one["estimate"]["candidates"].as_array().unwrap().clone();
        assert_eq!(candidates.len(), 1, "{one}");
        let defined = s.define_compression(&json!({
            "name": "cv", "label": "customer", "path": "cv", "minBytes": 64, "dictionary": false
        }));
        for key in [
            "compressedValues",
            "plainBytes",
            "storedBytes",
            "dictionaryBytes",
        ] {
            assert_eq!(defined["rule"]["stats"][key], candidates[0][key], "{key}");
        }
        let again = s.estimate_compression(&json!({"label": "customer", "path": "cv"}));
        assert_eq!(again["estimate"]["current"]["name"], "cv", "{again}");
        assert_eq!(
            again["estimate"]["current"]["stats"]["storedBytes"],
            candidates[0]["storedBytes"]
        );
    }

    #[test]
    fn an_estimate_spec_has_field_errors() {
        let s = session_with_text();
        let fields = |spec: Value| -> Vec<String> {
            let answer = s.estimate_compression(&spec);
            assert_eq!(answer["ok"], false, "{answer}");
            answer["errors"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["field"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(fields(json!({"path": "cv"})), ["label"]);
        assert_eq!(
            fields(json!({"label": "customer", "path": "a[0]"})),
            ["path"]
        );
        assert_eq!(
            fields(json!({"label": "customer", "path": "cv", "minBytes": 0})),
            ["minBytes"]
        );
        assert_eq!(
            fields(json!({
                "label": "customer", "path": "cv", "element": "node", "codec": "zip",
                "dictionary": 1
            })),
            ["element", "codec", "dictionary"]
        );
        assert_eq!(
            fields(json!({"label": "customer", "path": "cv", "name": "x"})),
            ["general"]
        );
        assert_eq!(fields(json!("cv")), ["general"]);
    }

    #[test]
    fn a_dedup_rule_is_defined_listed_and_estimated() {
        let mut s = Session::new();
        let out = s.execute(
            r#"for i in 0..12 { g.add_v("symbol").property("path", "crates/graphersal/src/graph/file_" + (i % 3) + ".rs").to_list(); }"#,
            &crate::RunOptions::default(),
        );
        assert!(out.contains("\"ok\":true"), "{out}");
        // Both codecs side by side, one request; dedup wins for three repeated paths.
        let answer = s.estimate_compression(&json!({
            "label": "symbol", "path": "path", "codec": ["lz4", "dedup"]
        }));
        assert_eq!(answer["ok"], true, "{answer}");
        let candidates = answer["estimate"]["candidates"].as_array().unwrap().clone();
        assert_eq!(candidates.len(), 14, "{answer}");
        let best = &candidates[answer["estimate"]["best"].as_u64().unwrap() as usize];
        assert_eq!(best["codec"], "dedup", "{answer}");
        assert_eq!(best["distinctValues"], 3);
        // The rule holds what the estimate said.
        let defined = s.define_compression(&json!({
            "name": "path", "label": "symbol", "path": "path", "codec": "dedup",
            "minBytes": best["minBytes"]
        }));
        assert_eq!(defined["ok"], true, "{defined}");
        assert_eq!(defined["rule"]["codec"], "dedup");
        assert_eq!(defined["rule"]["dictionary"], false);
        for key in [
            "compressedValues",
            "plainBytes",
            "storedBytes",
            "distinctValues",
        ] {
            assert_eq!(defined["rule"]["stats"][key], best[key], "{key}");
        }
        // A dictionary does not go with dedup: a field error on dictionary, with help.
        let refused = s.define_compression(&json!({
            "name": "other", "label": "symbol", "path": "doc", "codec": "dedup", "dictionary": true
        }));
        assert_eq!(refused["errors"][0]["field"], "dictionary", "{refused}");
        assert!(
            refused["errors"][0]["help"]
                .as_str()
                .unwrap()
                .contains("dedup")
        );
        let refused = s.estimate_compression(&json!({
            "label": "symbol", "path": "path", "codec": ["dedup", 3]
        }));
        assert_eq!(refused["errors"][0]["field"], "codec", "{refused}");
        // The reads stay plain.
        let out = s.execute(
            r#"g.v().has_label("symbol").values("path").dedup().count()"#,
            &crate::RunOptions::default(),
        );
        assert!(out.contains("3"), "{out}");
    }

    /// The modern graph twice: as a `TraversalGraph` and wrapped in the `Forwarding` storage
    /// (not `TraversalGraph`, own handle types), for the generic-helper comparisons.
    fn modern_pair() -> (
        TraversalGraph,
        graphersal_storage_tests::forwarding::Forwarding,
    ) {
        (
            TraversalGraph::tinkerpop_modern(),
            graphersal_storage_tests::forwarding::Forwarding::new(
                TraversalGraph::tinkerpop_modern(),
            ),
        )
    }

    #[test]
    fn rules_on_a_foreign_storage_equal_those_of_a_traversal_graph() {
        let (mut tg, mut fw) = modern_pair();
        let cv = "Senior engineer, led the platform team in Rust. ".repeat(20);
        let mut properties = PropertyMap::default();
        properties.insert("cv", cv.as_str().into());
        tg.add_vertex(None, vec!["customer".into()], properties.clone())
            .unwrap();
        fw.add_vertex(None, vec!["customer".into()], properties)
            .unwrap();
        let rule = || {
            let rule = CompressionDefinition::with_path(
                ElementKind::Vertex,
                "customer",
                "cv".parse().unwrap(),
            )
            .with_min_bytes(64);
            Definition::compression("cv", rule)
        };
        store_compression(&mut tg, rule(), false, None).unwrap();
        store_compression(&mut fw, rule(), false, None).unwrap();
        assert_eq!(compressions_in(&tg).unwrap(), compressions_in(&fw).unwrap());
        assert_eq!(
            compression_in(&fw, "cv").unwrap()["stats"]["compressedValues"],
            1
        );
        assert_eq!(compression_in(&fw, "none").unwrap(), Value::Null);
        recompress_in(&mut tg, "cv").unwrap();
        recompress_in(&mut fw, "cv").unwrap();
        assert_eq!(
            compression_in(&tg, "cv").unwrap(),
            compression_in(&fw, "cv").unwrap()
        );
        assert!(recompress_in(&mut fw, "none").is_err());
        // A storage that reports no stats (no compressed properties): the entry says `null`.
        let entry = entry("cv", rule().as_compression().unwrap(), None);
        assert_eq!(entry["stats"], Value::Null);
        assert_eq!(entry["path"], "$.cv");
    }

    /// A rule of a newer version (an unknown codec): listed with the codec as read, not
    /// applied, with the reason; recompress names the reason.
    #[test]
    fn a_rule_of_a_newer_version_is_listed_not_applied() {
        let mut map = PropertyMap::default();
        map.insert("label", "customer".into());
        map.insert("path", "cv".into());
        map.insert("codec", "zstd".into());
        let rule = CompressionDefinition::from_payload(&map).unwrap();
        let mut g = TraversalGraph::new();
        g.set_definition(Definition::compression("cv", rule))
            .unwrap();
        let listed = compression_in(&g, "cv").unwrap();
        assert_eq!(listed["codec"], "zstd");
        assert_eq!(listed["served"], false);
        assert_eq!(listed["stats"], Value::Null);
        assert!(
            listed["reason"].as_str().unwrap().contains("'zstd'"),
            "{listed}"
        );
        let err = recompress_in(&mut g, "cv").unwrap_err();
        assert!(
            err.to_string().contains("not known to this version"),
            "{err}"
        );
        // A rule this version applies is served, without a reason.
        let known = CompressionDefinition::new(ElementKind::Vertex, "customer", "bio").unwrap();
        g.set_definition(Definition::compression("bio", known))
            .unwrap();
        let listed = compression_in(&g, "bio").unwrap();
        assert_eq!(listed["served"], true);
        assert_eq!(listed["reason"], Value::Null);
    }
}
