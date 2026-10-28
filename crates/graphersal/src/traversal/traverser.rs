use crate::error::{TraverserError, TraverserResult};
use crate::graph::GraphStorage;
use crate::traversal::TraverserContext;
use crate::traversal::optimizer::path_analysis::PathMode;
use crate::value::{PathArena, PathRef, PropertyHandleKind, SackRef, TraverserValue};

/// One item flowing through a traversal: its current value and a handle to its path.
///
/// The path lives in the path arena of the execution, so cloning a traverser copies the value
/// handle and four bytes.
pub(crate) struct Traverser<S: GraphStorage> {
    /// The current object.
    pub value: TraverserValue<S>,
    pub(crate) path: PathRef,
    /// Multiplicity of this traverser: it stands for `bulk` identical traversers (TinkerPop
    /// `Traverser.bulk()`). Always at least 1. Values above 1 exist only after a merge point
    /// (see `traversal/bulk.rs`).
    pub(crate) bulk: u64,
    /// Handle of this traverser's sack in the execution's sack arena; `SackRef::NONE` (reads as
    /// `null`) when the query uses no `withSack`. Four bytes that fit the padding after `path`.
    pub(crate) sack: SackRef,
}

impl<S: GraphStorage> std::fmt::Debug for Traverser<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Traverser")
            .field("value", &self.value)
            .field("path", &self.path)
            .field("bulk", &self.bulk)
            .field("sack", &self.sack)
            .finish()
    }
}

impl<S: GraphStorage> Clone for Traverser<S> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            path: self.path,
            bulk: self.bulk,
            sack: self.sack,
        }
    }
}

impl<S: GraphStorage> Traverser<S> {
    /// Creates a traverser with an empty path, a bulk of 1 and no sack (it reads as `null`).
    ///
    /// Use a traversal source's `with_sack` for streams that carry a sack.
    #[cfg(test)]
    pub(crate) fn from_value(value: TraverserValue<S>) -> Self {
        Self {
            value,
            path: PathRef::NONE,
            bulk: 1,
            sack: SackRef::NONE,
        }
    }

    /// How many identical traversers this one stands for (TinkerPop `Traverser.bulk()`).
    ///
    /// Always at least 1; it is greater than 1 only for a traverser that merged equal
    /// traversers at a merge point.
    pub fn bulk(&self) -> u64 {
        self.bulk
    }

    /// A traverser *generated* by a start step or a reducing barrier (TinkerPop
    /// `TraverserGenerator.generate`): bulk 1, an empty path and the *initial* sack of the
    /// execution.
    #[inline]
    pub(crate) fn start(context: &TraverserContext<'_, S>, value: TraverserValue<S>) -> Self {
        Self {
            value,
            path: PathRef::NONE,
            bulk: 1,
            sack: context.initial_sack,
        }
    }

    /// A traverser derived from `self` (TinkerPop `split`): the new value with the same path,
    /// bulk and sack.
    #[inline]
    pub(crate) fn split(&self, value: TraverserValue<S>) -> Self {
        Self {
            value,
            path: self.path,
            bulk: self.bulk,
            sack: self.sack,
        }
    }

    /// A traverser with an explicit path, bulk and sack.
    #[inline]
    pub(crate) fn with_parts(
        value: TraverserValue<S>,
        path: PathRef,
        bulk: u64,
        sack: SackRef,
    ) -> Self {
        Self {
            value,
            path,
            bulk,
            sack,
        }
    }

    /// Returns the schema carried by this traverser, if its value is one.
    #[cfg(test)]
    pub(crate) fn as_schema(&self) -> Option<&crate::graph::schema::GraphSchema> {
        self.value.as_schema()
    }

    /// Records the current value as a new position of the path when `mode` is `Full`, and drops
    /// the path when nothing downstream reads it. In `Labels` mode the path is left untouched:
    /// only `as()` records positions there.
    #[inline(always)]
    pub(crate) fn extend_path(
        &mut self,
        paths: &mut PathArena<S>,
        mode: &PathMode,
    ) -> TraverserResult<()> {
        match mode {
            PathMode::Full => {
                self.path = paths.push(self.path, self.value.clone(), None)?;
            }
            PathMode::Labels(_) => {}
            PathMode::None => self.path = PathRef::NONE,
        }
        Ok(())
    }

    /// Keeps the path of a traverser that passes through a step unchanged, or drops it when
    /// nothing downstream reads it.
    #[inline(always)]
    pub(crate) fn keep_path(&mut self, mode: &PathMode) {
        if mode.is_none() {
            self.path = PathRef::NONE;
        }
    }

    /// Starts a new path at the current value, for steps that create traversers (start steps,
    /// barriers): the first position in `Full` mode, an empty path otherwise.
    #[inline(always)]
    pub(crate) fn start_path(
        mut self,
        paths: &mut PathArena<S>,
        mode: &PathMode,
    ) -> TraverserResult<Self> {
        self.path = PathRef::NONE;
        if matches!(mode, PathMode::Full) {
            self.path = paths.push(PathRef::NONE, self.value.clone(), None)?;
        }
        Ok(self)
    }

    /// The type name a string step reports when [`as_string`](Self::as_string) found no string.
    ///
    /// Reads the stored value's type in place, without materializing it: a property that is
    /// absent is `null`, a stored value reports its [`ValueType`](crate::graph::ValueType) name
    /// (`uuid`, `int64`, ...), and a non-property stream value reports its stream kind. A value
    /// the storage cannot read reports [`UNREADABLE_DETAIL`](crate::error::UNREADABLE_DETAIL):
    /// the string step's own error is the one to report, only without the type.
    pub(crate) fn observed_type_name(
        context: &TraverserContext<'_, S>,
        traverser_value: &TraverserValue<S>,
    ) -> String {
        Self::read_observed_type_name(context, traverser_value).unwrap_or_else(|error| {
            crate::error::unreadable_detail("the type of a value", &error);
            crate::error::UNREADABLE_DETAIL.to_string()
        })
    }

    /// [`Self::observed_type_name`], `Err` when the storage cannot read the value.
    fn read_observed_type_name(
        context: &TraverserContext<'_, S>,
        traverser_value: &TraverserValue<S>,
    ) -> Result<String, crate::error::GraphError> {
        let stored = match traverser_value {
            TraverserValue::Computed(value, _) => Some(value),
            TraverserValue::VertexProperty(prop, vertex_index, kind) => {
                match kind {
                    PropertyHandleKind::Id | PropertyHandleKind::Label => {
                        return Ok("string".to_string());
                    }
                    PropertyHandleKind::Labels => return Ok("array".to_string()),
                    PropertyHandleKind::Element | PropertyHandleKind::Value => {}
                }
                match context
                    .storage
                    .vertex_properties(*vertex_index)?
                    .and_then(|p| p.get(prop.as_str()))
                {
                    Some(value) => Some(value),
                    None => return Ok("null".to_string()),
                }
            }
            TraverserValue::EdgeProperty(prop, edge_index, kind) => {
                match kind {
                    PropertyHandleKind::Id | PropertyHandleKind::Label => {
                        return Ok("string".to_string());
                    }
                    PropertyHandleKind::Labels => return Ok("array".to_string()),
                    PropertyHandleKind::Element | PropertyHandleKind::Value => {}
                }
                match context
                    .storage
                    .edge_properties(*edge_index)?
                    .and_then(|p| p.get(prop.as_str()))
                {
                    Some(value) => Some(value),
                    None => return Ok("null".to_string()),
                }
            }
            _ => None,
        };
        Ok(match stored {
            Some(value) => crate::graph::schema::value_type_name(value),
            None => traverser_value.type_name().to_string(),
        })
    }

    /// The string a stream value holds, borrowed: a string property, an element id or label
    /// read through an `id()`/`label()` handle, or a computed string. Every value stored in the
    /// graph is already backed by a `&str` reachable from `context`, so every case borrows at
    /// zero cost; a vertex/edge without a stored id is an `InvalidElementState` error, never a
    /// made-up id. The [`std::borrow::Cow`] return type leaves room for a future case that has
    /// to allocate.
    pub(crate) fn as_string_ref<'ctx>(
        context: &'ctx TraverserContext<'_, S>,
        traverser_value: &'ctx TraverserValue<S>,
    ) -> TraverserResult<Option<std::borrow::Cow<'ctx, str>>> {
        use std::borrow::Cow;

        match traverser_value {
            TraverserValue::Computed(value, _) => Ok(value.text()),
            TraverserValue::VertexProperty(prop, vertex_index, kind) => {
                if *kind == PropertyHandleKind::Label {
                    Ok(context
                        .storage
                        .vertex_label(*vertex_index)?
                        .map(Cow::Borrowed))
                } else if *kind == PropertyHandleKind::Id {
                    let id = Cow::Borrowed(crate::graph::storage::stored_vertex_id(
                        &*context.storage,
                        *vertex_index,
                    )?);
                    Ok(Some(id))
                } else if !kind.reads_property() {
                    Ok(None)
                } else {
                    let value = context
                        .storage
                        .vertex_properties(*vertex_index)?
                        .and_then(|p| p.get(prop.as_str()))
                        .and_then(|p| p.text());
                    Ok(value)
                }
            }
            TraverserValue::EdgeProperty(prop, edge_index, kind) => {
                if *kind == PropertyHandleKind::Label {
                    Ok(context.storage.edge_label(*edge_index)?.map(Cow::Borrowed))
                } else if *kind == PropertyHandleKind::Id {
                    let id = Cow::Borrowed(crate::graph::storage::stored_edge_id(
                        &*context.storage,
                        *edge_index,
                    )?);
                    Ok(Some(id))
                } else if !kind.reads_property() {
                    Ok(None)
                } else {
                    let value = context
                        .storage
                        .edge_properties(*edge_index)?
                        .and_then(|p| p.get(prop.as_str()))
                        .and_then(|p| p.text());
                    Ok(value)
                }
            }
            t => Err(Box::new(TraverserError::cast_exception(
                &["computed", "vertexProperty", "edgeProperty"],
                t,
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Traverser;
    use crate::graph::TraversalGraph;
    use crate::value::TraverserValue;
    use std::mem::size_of;

    /// Rule 20: the value (64 bytes), a 4-byte path handle, a 4-byte sack handle in the former
    /// padding and an 8-byte bulk; traversers stay cheap to clone and to buffer.
    #[test]
    fn traverser_stays_80_bytes() {
        type G = TraversalGraph;
        assert_eq!(size_of::<Traverser<G>>(), 80);
        assert!(size_of::<Traverser<G>>() <= size_of::<TraverserValue<G>>() + 16);
    }
}
