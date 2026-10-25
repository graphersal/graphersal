//! `embed()` / `embed(key)`: a text becomes a vector through an embedder of the registry the
//! traversal was given (Graphersal extension, book page "Vectors", section "Embeddings").

use std::sync::Arc;

use crate::graph::GraphStorage;
use crate::traversal::step::map_steps::step_similarity::VectorKey;
use crate::traversal::step::map_steps::temporal::{describe, held_value, replaced};
use crate::value::vector::embed::checked_embed;
use crate::value::{EmbedContext, EmbedError, Embedder};
use crate::*;

/// `embed([key][, embedder])`: the incoming string (or the string property `key` of the
/// incoming element) embedded by the embedder `requested` (`None`: the registry's default).
/// Batches of the embedder's `max_batch` traversers make one call each; an element without the
/// property is dropped; a value that is not a string fails.
#[derive(Clone)]
struct StepEmbed {
    key: Option<VectorKey>,
    requested: Option<CompactString>,
    /// Bound by `run()` before the permission check.
    embedder: Option<Arc<dyn Embedder>>,
}

impl fmt::Display for StepEmbed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("embed(")?;
        if let Some(key) = &self.key {
            f.write_str(&key.literal())?;
        }
        let name = match &self.embedder {
            Some(embedder) => Some(embedder.info().name.as_str()),
            None => self.requested.as_deref(),
        };
        if let Some(name) = name {
            if self.key.is_some() {
                f.write_str(", ")?;
            }
            write!(f, "embedder: {}", crate::value::literal::rhai_string(name))?;
        }
        f.write_str(")")
    }
}

fn embed_step<'g, S: GraphStorage + 'g>(
    key: Option<PropKey>,
    embedder: Option<&str>,
) -> GraphStep<'g, S> {
    GraphStep::Map(
        "embed",
        Box::new(StepEmbed {
            key: key.map(VectorKey::new),
            requested: embedder.map(CompactString::new),
            embedder: None,
        }),
    )
}

/// The traversal error of `error`; a cancelled embedding becomes the execution's own
/// cancellation or timeout.
pub(crate) fn embed_failure<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    error: EmbedError,
) -> Box<TraverserError> {
    if error == EmbedError::Cancelled
        && let Some(deadline) = &context.deadline
        && let Err(stopped) = deadline.check()
    {
        return stopped;
    }
    Box::new(TraverserError::Embed(error))
}

impl StepEmbed {
    /// The text of `value`: `Ok(None)` for an element without the property (dropped).
    fn text_of<S: GraphStorage>(
        &self,
        context: &TraverserContext<'_, S>,
        value: &TraverserValue<S>,
    ) -> TraverserResult<Option<String>> {
        let text = match &self.key {
            Some(key) => {
                match key.peek(context, value, |stored| {
                    stored
                        .with_text(str::to_string)
                        .ok_or_else(|| describe(stored))
                })? {
                    None => return Ok(None),
                    Some(text) => text,
                }
            }
            None => match held_value(context, value)? {
                Ok(held) => held
                    .with_text(str::to_string)
                    .ok_or_else(|| describe(held.as_ref())),
                Err(kind) => Err(format!("a {kind}")),
            },
        };
        match text {
            Ok(text) => {
                context.check_string_bytes(text.len())?;
                Ok(Some(text))
            }
            Err(found) => Err(Box::new(TraverserError::Embed(EmbedError::not_text(found)))),
        }
    }

    fn embedder(&self) -> TraverserResult<&Arc<dyn Embedder>> {
        self.embedder.as_ref().ok_or_else(|| {
            Box::new(TraverserError::QueryExecution {
                query: self.to_string(),
                reason: "embed() ran without a resolved embedder".to_string(),
            })
        })
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepEmbed {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        if let Some(key) = self.key.as_ref().and_then(|key| key.top_level()) {
            visit(Some(key), crate::graph::workload::AccessKind::Read);
        }
    }
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        self.process_filter(context, source)?.ok_or_else(|| {
            Box::new(TraverserError::QueryExecution {
                query: self.to_string(),
                reason: "embed() dropped a traverser in a context that cannot filter".to_string(),
            })
        })
    }

    fn process_filter(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        Ok(self.process_batch(context, vec![source])?.pop())
    }

    fn batch_size(&self) -> usize {
        self.embedder
            .as_ref()
            .map_or(1, |embedder| embedder.info().max_batch.max(1))
    }

    fn process_batch(
        &self,
        context: &mut TraverserContext<'_, S>,
        sources: Vec<Traverser<S>>,
    ) -> TraverserResult<Vec<Traverser<S>>> {
        let embedder = self.embedder()?.clone();
        let mut kept = Vec::with_capacity(sources.len());
        let mut texts = Vec::with_capacity(sources.len());
        for source in sources {
            if let Some(text) = self.text_of(context, &source.value)? {
                texts.push(text);
                kept.push(source);
            }
        }
        if kept.is_empty() {
            return Ok(kept);
        }
        // Each distinct traverser is embedded once, whatever its bulk.
        let mut out = Vec::with_capacity(kept.len());
        let mut kept = kept.into_iter();
        for texts in texts.chunks(embedder.info().max_batch.max(1)) {
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            let vectors = checked_embed(
                &*embedder,
                &refs,
                &EmbedContext::of_deadline(context.deadline.as_ref()),
            )
            .map_err(|error| embed_failure(context, error))?;
            context.tick_n(refs.len())?;
            // The vectors first: `zip` stops at the end of this chunk's vectors without taking
            // the next chunk's traverser.
            for (vector, source) in vectors.into_iter().zip(kept.by_ref()) {
                out.push(replaced(
                    source,
                    TraverserValue::Computed(ElementProperty::Vector(vector), None),
                ));
            }
        }
        Ok(out)
    }

    fn may_filter(&self) -> bool {
        true
    }

    fn materializes(&self) -> bool {
        true
    }

    fn embedder_request(&self) -> Option<Option<CompactString>> {
        Some(self.requested.clone())
    }

    fn bind_embedder(&mut self, embedder: Arc<dyn Embedder>) {
        self.embedder = Some(embedder);
    }

    fn embedder_name(&self) -> Option<&str> {
        match &self.embedder {
            Some(embedder) => Some(embedder.info().name.as_str()),
            None => self.requested.as_deref(),
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `embed(key[, #{embedder}])`: the string property `key` (a name or a
    /// [`JsonPath`](crate::value::JsonPath)) of the incoming element embedded as a vector by the
    /// embedder `embedder` of the traversal's registry
    /// ([`with_embedders`](Self::with_embedders); `None`: its default, `hash-ngram/1` unless
    /// the host chose another). A Graphersal extension (book page "Vectors").
    ///
    /// An element without the property is dropped (like `values(key)`); a value that is not a
    /// string fails with [`EmbedError::NotText`]. The step hands the embedder batches of its
    /// `max_batch` traversers (each traverser once, whatever its bulk) and asks `Execute` on
    /// the embedder ([`Resource::Embedder`](crate::auth::Resource::Embedder)).
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let g = graph.read();
    /// let vectors = g.traversal().v(None::<()>).embed("name", None).to_list().unwrap();
    /// assert_eq!(vectors.len(), 6);
    /// ```
    pub fn embed(self, key: impl Into<PropKey>, embedder: Option<&str>) -> Self {
        self.push_step(embed_step(Some(key.into()), embedder))
    }

    /// `embed([#{embedder}])`: the incoming string embedded as a vector, as in
    /// [`embed`](Self::embed); any other value fails.
    pub fn embed_value(self, embedder: Option<&str>) -> Self {
        self.push_step(embed_step(None, embedder))
    }
}

impl __ {
    /// Anonymous `embed(key[, #{embedder}])`; see [`GraphTraversalSource::embed`].
    pub fn embed<'g, S: GraphStorage + 'g>(
        key: impl Into<PropKey>,
        embedder: Option<&str>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().embed(key, embedder)
    }

    /// Anonymous `embed([#{embedder}])`; see [`GraphTraversalSource::embed_value`].
    pub fn embed_value<'g, S: GraphStorage + 'g>(
        embedder: Option<&str>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().embed_value(embedder)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `embed(key[, #{embedder}])`; see [`GraphTraversalSource::embed`].
    pub fn embed(self, key: impl Into<PropKey>, embedder: Option<&str>) -> Self {
        self.push_step(embed_step(Some(key.into()), embedder))
    }

    /// `embed([#{embedder}])`; see [`GraphTraversalSource::embed_value`].
    pub fn embed_value(self, embedder: Option<&str>) -> Self {
        self.push_step(embed_step(None, embedder))
    }
}
