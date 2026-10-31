//! Per-execution storage for traverser sacks.
//!
//! A traverser does not own its sack value. It holds a [`SackRef`] into the [`SackArena`] of the
//! execution it belongs to. Sack values are immutable: updating a sack interns the new value and
//! replaces the handle of that one traverser, so deriving a traverser (TinkerPop `split`) copies
//! four bytes and comparing the sacks of two traversers at a merge point is a four byte compare.
//!
//! Scalars are interned, so equal scalar sacks share one handle (`1` and `1.0` are different
//! values and get different handles; `0.0` and `-0.0` are one value because `Number`
//! canonicalizes them; every `NaN` shares one handle).
//! Containers are appended without interning: each gets a fresh handle, so two container sacks
//! never compare equal and such traversers never merge automatically.
//!
//! A traverser without a sack carries [`SackRef::NONE`], which reads as `null`; a query without
//! `withSack` never touches the arena.

use compact_str::CompactString;
use fnv::FnvHashMap;
use std::collections::hash_map::Entry;

use crate::error::{TraverserError, TraverserResult, ValueError};
use crate::value::ElementProperty;

/// A handle to a sack value inside a [`SackArena`].
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct SackRef(u32);

impl SackRef {
    /// The absent sack: reads as `null`.
    pub(crate) const NONE: SackRef = SackRef(u32::MAX);

    /// Returns `true` when this handle points at no value.
    #[inline]
    pub(crate) fn is_none(self) -> bool {
        self == Self::NONE
    }
}

impl Default for SackRef {
    fn default() -> Self {
        Self::NONE
    }
}

/// The sack configuration of a traversal source (`withSack`).
#[derive(Clone, Debug)]
pub(crate) struct SackSpec {
    /// The initial sack of every generated traverser.
    pub(crate) initial: ElementProperty,
    /// The merge operator of `withSack(initial, Operator)`; `None` for `withSack(initial)`.
    pub(crate) merge: Option<crate::value::Operator>,
}

/// Interning key of a scalar sack value. Floats are keyed by their bits.
#[derive(Clone, PartialEq, Eq, Hash)]
enum ScalarKey {
    Null,
    Bool(bool),
    Int(i64),
    FloatBits(u64),
    Str(CompactString),
    Uuid(u128),
    /// Every field of the value: two datetimes of one instant but different offsets are equal
    /// values, yet interned apart so a sack keeps the offset it was given. Boxed: inline, the
    /// 32 bytes grew every key from 32 to 48 bytes and slowed sack merges by about 13 %.
    Temporal(Box<crate::value::temporal::TemporalBits>),
}

impl ScalarKey {
    fn of(value: &ElementProperty) -> Option<ScalarKey> {
        Some(match value {
            ElementProperty::Null => ScalarKey::Null,
            ElementProperty::Boolean(b) => ScalarKey::Bool(*b),
            ElementProperty::Number(n) => match n.as_i64() {
                Some(i) => ScalarKey::Int(i),
                None => {
                    let f = n.as_f64().unwrap_or(f64::NAN);
                    ScalarKey::FloatBits(if f.is_nan() {
                        f64::NAN.to_bits()
                    } else {
                        f.to_bits()
                    })
                }
            },
            ElementProperty::String(s) => ScalarKey::Str(s.clone()),
            ElementProperty::Compressed(text) => ScalarKey::Str(text.text()),
            ElementProperty::Deduped(text) => ScalarKey::Str(text.as_str().into()),
            ElementProperty::Uuid(u) => ScalarKey::Uuid(*u),
            ElementProperty::Temporal(t) => ScalarKey::Temporal(Box::new(t.bits())),
            // Not interned: a vector gets a fresh handle like a container.
            ElementProperty::Vector(_)
            | ElementProperty::Blob(_)
            | ElementProperty::Array(_)
            | ElementProperty::Object(_)
            | ElementProperty::Map(_)
            | ElementProperty::Vertex(_)
            | ElementProperty::Edge(_) => return None,
        })
    }
}

static NULL: ElementProperty = ElementProperty::Null;

/// The cast error of a vertex or edge sack (a sack holds data, not a graph element).
#[cold]
fn element_sack(value: &ElementProperty) -> Box<TraverserError> {
    let actual = if matches!(value, ElementProperty::Vertex(_)) {
        "vertex"
    } else {
        "edge"
    };
    ValueError::CastException {
        expected: "scalar, array or object".to_string(),
        actual: actual.to_string(),
        value: None,
        origin: None,
    }
    .into()
}

/// Arena of sack values owned by one traversal execution.
///
/// Nested traversals run on the same context and share the arena.
pub(crate) struct SackArena {
    values: Vec<ElementProperty>,
    intern: FnvHashMap<ScalarKey, u32>,
    /// Maximum number of values; `u32::MAX - 1` in production, smaller in tests.
    capacity: u32,
}

impl Default for SackArena {
    fn default() -> Self {
        Self::new()
    }
}

impl SackArena {
    /// Creates an empty arena. It allocates nothing until the first value is interned.
    pub(crate) fn new() -> Self {
        Self {
            values: Vec::new(),
            intern: FnvHashMap::default(),
            capacity: u32::MAX - 1,
        }
    }

    /// Creates an arena that holds at most `capacity` values, so exhaustion can be tested.
    #[cfg(test)]
    pub(crate) fn with_capacity_limit(capacity: u32) -> Self {
        Self {
            capacity,
            ..Self::new()
        }
    }

    /// Stores `value` and returns its handle: the existing handle for an equal scalar, a fresh
    /// one for a container.
    ///
    /// Fails with [`TraverserError::SackArenaExhausted`] when the arena is full and with a
    /// cast error for a vertex or edge value (a sack holds data, not a graph element).
    pub(crate) fn intern(&mut self, value: ElementProperty) -> TraverserResult<SackRef> {
        let index = self.values.len();
        // A scalar: one hash and one probe for both the lookup and the insertion (a sack merge
        // operator interns every combined value, so this is the merge loop's hottest call).
        if let Some(key) = ScalarKey::of(&value) {
            return match self.intern.entry(key) {
                Entry::Occupied(slot) => Ok(SackRef(*slot.get())),
                Entry::Vacant(slot) => {
                    if index >= self.capacity as usize {
                        return Err(self.exhausted());
                    }
                    self.values.push(value);
                    slot.insert(index as u32);
                    Ok(SackRef(index as u32))
                }
            };
        }
        if let ElementProperty::Vertex(_) | ElementProperty::Edge(_) = &value {
            return Err(element_sack(&value));
        }
        if index >= self.capacity as usize {
            return Err(self.exhausted());
        }
        self.values.push(value);
        Ok(SackRef(index as u32))
    }

    /// The [`TraverserError::SackArenaExhausted`] error of this arena.
    #[cold]
    fn exhausted(&self) -> Box<TraverserError> {
        Box::new(TraverserError::SackArenaExhausted {
            capacity: u64::from(self.capacity),
        })
    }

    /// The value behind `handle`; [`SackRef::NONE`] reads as `null`.
    #[inline]
    pub(crate) fn get(&self, handle: SackRef) -> &ElementProperty {
        self.values.get(handle.0 as usize).unwrap_or(&NULL)
    }

    /// Number of stored values.
    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.values.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Number;

    fn int(i: i64) -> ElementProperty {
        ElementProperty::Number(Number::from_i64(i))
    }
    fn float(f: f64) -> ElementProperty {
        ElementProperty::Number(Number::from_f64(f))
    }

    #[test]
    fn equal_scalars_share_a_handle() {
        let mut arena = SackArena::new();
        let a = arena.intern(int(5)).unwrap();
        let b = arena.intern(int(5)).unwrap();
        assert_eq!(a, b);
        assert_eq!(arena.len(), 1);
        let s1 = arena.intern(ElementProperty::String("x".into())).unwrap();
        let s2 = arena.intern(ElementProperty::String("x".into())).unwrap();
        assert_eq!(s1, s2);
    }

    #[test]
    fn int_and_float_differ_and_signed_zero_is_canonical() {
        let mut arena = SackArena::new();
        assert_ne!(
            arena.intern(int(1)).unwrap(),
            arena.intern(float(1.0)).unwrap()
        );
        // `Number` canonicalizes `-0.0` to `0.0` engine-wide, so they cannot differ here.
        assert_eq!(
            arena.intern(float(0.0)).unwrap(),
            arena.intern(float(-0.0)).unwrap()
        );
        assert_eq!(
            arena.intern(float(f64::NAN)).unwrap(),
            arena.intern(float(-f64::NAN)).unwrap()
        );
    }

    #[test]
    fn containers_never_share_a_handle() {
        let mut arena = SackArena::new();
        let a = arena.intern(ElementProperty::Array(vec![int(1)])).unwrap();
        let b = arena.intern(ElementProperty::Array(vec![int(1)])).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn none_reads_null() {
        let arena = SackArena::new();
        assert!(matches!(arena.get(SackRef::NONE), ElementProperty::Null));
        assert!(SackRef::NONE.is_none());
    }

    #[test]
    fn exhaustion_is_an_error() {
        let mut arena = SackArena::with_capacity_limit(2);
        arena.intern(int(1)).unwrap();
        arena.intern(int(2)).unwrap();
        assert!(
            arena.intern(int(1)).is_ok(),
            "an existing scalar needs no slot"
        );
        let err = arena.intern(int(3)).unwrap_err();
        assert!(matches!(
            *err,
            TraverserError::SackArenaExhausted { capacity: 2 }
        ));
    }
}
