//! The embedders of the models a Store keeps (features `persist` + `model2vec`, native only;
//! format spec 4.5, book page "Embedding Models in a Store"): at open, a front end asks
//! [`Store::stored_embedders`] for a registry holding its own embedders plus every stored model
//! none of them provides (the ones the schema names under the schema's identity). Its own embedders win: a stored model is
//! loaded only for an identity no registered embedder has.

use std::sync::Arc;

use compact_str::CompactString;

use super::files::DirPath;
use super::{Store, models};
use crate::graph::PersistentStorage;
use crate::graph::schema::GraphSchema;
use crate::persist::PersistError;
use crate::value::vector::embed::identity::parse_identity;
use crate::value::vector::embed::provenance::schema_models;
use crate::value::{Embedder, Embedders, Model2Vec, Model2VecOptions};

/// What [`Store::stored_embedders`] found: the registry to use, the models it loaded and what
/// the schema names that neither the registry nor the store provides.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StoredEmbedders {
    /// The given registry with the loaded models added (its default unchanged).
    pub embedders: Arc<Embedders>,
    /// The stored models loaded: (registry name, model identity).
    pub loaded: Vec<(String, String)>,
    /// One line per model the schema names that is neither registered nor usable from the
    /// store, with the command that adds it (front ends print them as notes).
    pub notes: Vec<String>,
}

impl<S: PersistentStorage> Store<S> {
    /// The registry `registered` plus the embedding models this store keeps that no embedder
    /// of `registered` provides: what a front end serves the store with (format spec 4.5).
    /// Every model the schema names (its vector properties' `embedder` identity,
    /// `model2vec/<repo>@<12 hex>`) is loaded under the identity the schema records; every
    /// other stored model under its own identity (it was put into the store to be used: the
    /// first `embed_property` with it needs it before the schema names it). An embedder the
    /// host registered for an identity always wins. A model the schema
    /// names but neither `registered` nor the store holds, or one whose files do not load, is a
    /// note with the command that adds it. Without a schema or models it returns `registered`
    /// unchanged. Fails when the schema cannot be read ([`PersistError::Graph`] holding
    /// [`GraphError::GraphViewHeld`](crate::error::GraphError::GraphViewHeld) on a thread that
    /// holds a unit of the graph beside another view) and when the store's model directory or a
    /// model's manifest lookup cannot be read ([`PersistError::Io`]); a stored model that does
    /// not load is a note, not an error.
    ///
    /// The loaded embedders are registered as the model's repository (`minishlab/potion-base-8M`),
    /// else as `model2vec-<12 hex>` (with `-<12 hex>` appended when the name is taken).
    pub fn stored_embedders(
        &self,
        registered: &Embedders,
    ) -> Result<StoredEmbedders, PersistError> {
        let schema = self
            .graph
            .read_checked()
            .map_err(PersistError::Graph)?
            .get_schema();
        let location = self.core.shared.dir.display();
        load(
            &self.core.shared.dir,
            schema.as_ref(),
            registered,
            &location,
        )
    }
}

/// [`Store::stored_embedders`] for the store at `dir` with the schema `schema`.
pub(crate) fn load(
    dir: &DirPath,
    schema: Option<&GraphSchema>,
    registered: &Embedders,
    location: &str,
) -> Result<StoredEmbedders, PersistError> {
    let mut found = StoredEmbedders {
        embedders: Arc::new(registered.clone()),
        loaded: Vec::new(),
        notes: Vec::new(),
    };
    let named = schema.map(schema_models).unwrap_or_default();
    let mut current = registered.clone();
    let mut added = Vec::new();
    // Every model the store keeps is registered too (it was put there to be used, and the first
    // `embed_property` with it needs it before the schema names it); a model the schema names
    // first, under the identity the schema records.
    let stored_models = models::list(dir)?;
    let mut used_ids: Vec<String> = Vec::new();
    for (identity, _) in &named {
        if let Some(id) = models::id_of(identity) {
            used_ids.push(id);
        }
    }
    for (identity, place) in named {
        if registered.by_model(&identity).is_some() {
            continue;
        }
        let Some(parsed) = parse_identity(&identity) else {
            found.notes.push(format!(
                "the schema names the embedding model {identity:?} ({place}), which no \
                 registered embedder provides: register an embedder of that model"
            ));
            continue;
        };
        let add_hint = match parsed.repo {
            Some(repo) => format!(
                "add it with `graphersal embed-model download {repo} --into {location}`, or \
                 pass it with --embedder NAME=<model_dir>"
            ),
            None => format!(
                "add it with `graphersal embed-model import <model_dir> --into {location}`, or \
                 pass it with --embedder NAME=<model_dir>"
            ),
        };
        let stored = models::find(dir, &identity)?;
        let Some(stored) = stored.filter(|m| m.problem.is_none()) else {
            found.notes.push(format!(
                "the schema names the embedding model {identity} ({place}), which is neither \
                 registered nor stored in {location}: {add_hint}"
            ));
            continue;
        };
        match load_one(
            dir,
            &stored,
            &identity,
            parsed.repo,
            parsed.normalize,
            &current,
        ) {
            Ok((name, embedder)) => match current.to_builder().with(embedder) {
                Ok(next) => {
                    current = next.build();
                    added.push((name, identity.clone()));
                }
                Err(err) => found.notes.push(format!(
                    "the stored model {identity} could not be registered: {err}"
                )),
            },
            Err(reason) => found.notes.push(format!(
                "the stored model {identity} ({place}) does not load: {reason}; remove it with \
                 `graphersal store models remove {location} {}` and {add_hint}",
                stored.id
            )),
        }
    }
    for stored in stored_models {
        if stored.problem.is_some() || used_ids.contains(&stored.id) {
            continue;
        }
        let identity = stored.identity.clone();
        if current.by_model(&identity).is_some() {
            continue;
        }
        let repo = stored.repo.clone();
        match load_one(dir, &stored, &identity, repo.as_deref(), None, &current) {
            Ok((name, embedder)) => match current.to_builder().with(embedder) {
                Ok(next) => {
                    current = next.build();
                    added.push((name, identity));
                }
                Err(err) => found.notes.push(format!(
                    "the stored model {identity} could not be registered: {err}"
                )),
            },
            Err(reason) => found.notes.push(format!(
                "the stored model {identity} does not load: {reason}; remove it with \
                 `graphersal store models remove {location} {}`",
                stored.id
            )),
        }
    }
    if !added.is_empty() {
        found.embedders = Arc::new(current);
        found.loaded = added;
    }
    Ok(found)
}

/// Loads the stored model `stored` for the schema's `identity` (its repository and
/// normalization): the registry name and the embedder.
fn load_one(
    dir: &DirPath,
    stored: &models::StoredModel,
    identity: &str,
    repo: Option<&str>,
    normalize: Option<bool>,
    registry: &Embedders,
) -> Result<(String, Arc<dyn Embedder>), String> {
    let file = |role: &str| -> Result<Vec<u8>, String> {
        let name = stored
            .files
            .iter()
            .find(|f| f.role == role)
            .map(|f| f.name.clone())
            .ok_or_else(|| format!("it has no {role} file"))?;
        models::read_file(dir, &stored.id, &name).map_err(|err| err.to_string())
    };
    let (config, tokenizer, weights) = (file("config")?, file("tokenizer")?, file("model")?);
    let name = registry_name(registry, repo, &stored.id);
    let mut options = Model2VecOptions::default();
    if let Some(repo) = repo {
        options = options.with_repo(repo);
    }
    if let Some(normalize) = normalize {
        options = options.with_normalize(normalize);
    }
    let model = Model2Vec::load_bytes(
        CompactString::from(name.as_str()),
        config,
        tokenizer,
        weights,
        &options,
        &format!("{} of the store", stored.id),
    )
    .map_err(|err| err.to_string())?;
    // A normalization override the config already has gives the plain identity: the
    // schema's identity is the one the vectors were made under, so it must match exactly.
    if model.info().model != identity {
        return Err(format!(
            "it loads as {}, not as the identity the schema names",
            model.info().model
        ));
    }
    Ok((name, Arc::new(model)))
}

/// The registry name of a loaded model: its repository, else its id; `-<12 hex>` appended to a
/// repository name that is taken or reserved.
fn registry_name(registry: &Embedders, repo: Option<&str>, id: &str) -> String {
    let taken = |name: &str| {
        registry.to_builder().is_taken(name)
            || name.len() > crate::value::vector::embed::MAX_EMBEDDER_NAME_BYTES
    };
    let hex = id.rsplit('-').next().unwrap_or(id);
    let candidates = match repo {
        Some(repo) => vec![repo.to_string(), format!("{repo}-{hex}"), id.to_string()],
        None => vec![id.to_string()],
    };
    candidates
        .into_iter()
        .find(|name| !taken(name))
        .unwrap_or_else(|| format!("{id}-{}", crate::value::uuid::new_v7() % 1_000_000))
}
