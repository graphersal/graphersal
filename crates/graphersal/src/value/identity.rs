//! Exact value identity: the equality and hash under which the dictionary of a `dedup`
//! compression rule (`value/dictionary.rs`) shares a value.
//!
//! The logical `==`/`Hash` of [`ElementProperty`] are not exact enough for sharing: they make
//! `1` equal to `1.0`, `-0.0` equal to `0.0`, two date-times of one instant at different
//! offsets equal, and an object equal to the same entries in another order. A shared value is
//! read back by every element that holds its code, so two values are shared only when they are
//! IDENTICAL: the same variant and sub-type, numbers by their bits, temporal values with their
//! offset and zone (`Temporal::identical`),
//! strings by bytes, arrays element by element and objects entry by entry IN ORDER (keys
//! included). Identical values hash alike ([`identity_hash`]).
//!
//! A storage-only string (`Compressed`, `Deduped`) is identical to the plain string of its
//! text; the dictionary only ever holds logical values.

use std::hash::{Hash, Hasher};

use super::ElementProperty;
use super::number::N;

/// Whether `a` and `b` are identical (module docs).
pub(crate) fn identity_eq(a: &ElementProperty, b: &ElementProperty) -> bool {
    use ElementProperty as E;
    match (a, b) {
        (E::Null, E::Null) => true,
        (E::Boolean(a), E::Boolean(b)) => a == b,
        (E::Number(a), E::Number(b)) => match (&a.n, &b.n) {
            (N::NegInt(a), N::NegInt(b)) => a == b,
            (N::Float(a), N::Float(b)) => a.0.to_bits() == b.0.to_bits(),
            (N::NegInt(_), N::Float(_)) | (N::Float(_), N::NegInt(_)) => false,
        },
        (E::Uuid(a), E::Uuid(b)) => a == b,
        (E::Temporal(a), E::Temporal(b)) => a.identical(b),
        (E::Vector(a), E::Vector(b)) => a.identical(b),
        (E::Blob(a), E::Blob(b)) => a == b,
        (E::Array(a), E::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| identity_eq(a, b))
        }
        (E::Object(a), E::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|((ka, va), (kb, vb))| ka == kb && identity_eq(va, vb))
        }
        (
            E::String(_) | E::Compressed(_) | E::Deduped(_),
            E::String(_) | E::Compressed(_) | E::Deduped(_),
        ) => match (a.text(), b.text()) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        },
        // Not property values (a graph refuses them); the logical equality is exact enough.
        (E::Vertex(_) | E::Edge(_) | E::Map(_), _) => a == b,
        (
            E::Null
            | E::Boolean(_)
            | E::Number(_)
            | E::Uuid(_)
            | E::Temporal(_)
            | E::Vector(_)
            | E::Blob(_)
            | E::Array(_)
            | E::Object(_)
            | E::String(_)
            | E::Compressed(_)
            | E::Deduped(_),
            _,
        ) => false,
    }
}

/// The hash of `value` under [`identity_eq`].
pub(crate) fn identity_hash<H: Hasher>(value: &ElementProperty, state: &mut H) {
    use ElementProperty as E;
    match value {
        E::Null => state.write_u8(0),
        E::Boolean(b) => {
            state.write_u8(1);
            b.hash(state);
        }
        E::Number(number) => match &number.n {
            N::NegInt(n) => {
                state.write_u8(2);
                n.hash(state);
            }
            N::Float(f) => {
                state.write_u8(3);
                f.0.to_bits().hash(state);
            }
        },
        E::Uuid(u) => {
            state.write_u8(4);
            u.hash(state);
        }
        E::Temporal(t) => {
            state.write_u8(5);
            t.identity_hash(state);
        }
        E::Vector(v) => {
            state.write_u8(6);
            v.hash(state);
        }
        E::Blob(b) => {
            state.write_u8(7);
            b.hash(state);
        }
        E::Array(values) => {
            state.write_u8(8);
            state.write_usize(values.len());
            values.iter().for_each(|v| identity_hash(v, state));
        }
        E::Object(map) => {
            state.write_u8(9);
            state.write_usize(map.len());
            for (key, v) in map.iter() {
                key.as_str().hash(state);
                identity_hash(v, state);
            }
        }
        E::String(_) | E::Compressed(_) | E::Deduped(_) => {
            state.write_u8(10);
            if let Some(text) = value.text() {
                text.as_ref().hash(state);
            }
        }
        E::Vertex(_) | E::Edge(_) | E::Map(_) => state.write_u8(11),
    }
}

/// [`identity_hash`] as one `u64` (one multiply per 8 bytes: the dictionaries hash every value
/// they code, a load all of them).
pub(crate) fn identity_hash_u64(value: &ElementProperty) -> u64 {
    let mut hasher = crate::graph::shape::MixHasher::default();
    identity_hash(value, &mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::PropertyMap;

    fn same(a: &ElementProperty, b: &ElementProperty) -> bool {
        let equal = identity_eq(a, b);
        if equal {
            assert_eq!(identity_hash_u64(a), identity_hash_u64(b), "{a:?} {b:?}");
        }
        equal
    }

    #[test]
    fn identity_is_stricter_than_equality() {
        let one = ElementProperty::from(1i64);
        let one_float = ElementProperty::from(1.0f64);
        assert!(!same(&one, &one_float));
        assert!(same(&one, &ElementProperty::from(1i64)));
        assert!(same(&one_float, &ElementProperty::from(1.0f64)));
        // One instant at two offsets: equal, not identical.
        use crate::value::temporal::{Temporal, TemporalKind};
        let at = |text: &str| {
            ElementProperty::Temporal(Temporal::parse_as(TemporalKind::DateTime, text).unwrap())
        };
        let (berlin, utc) = (at("2024-01-01T10:00:00+02:00"), at("2024-01-01T08:00:00Z"));
        assert_eq!(berlin, utc);
        assert!(!same(&berlin, &utc));
        assert!(same(&berlin, &at("2024-01-01T10:00:00+02:00")));
        let mut ab = PropertyMap::new();
        ab.insert("a", 1i64.into());
        ab.insert("b", 2i64.into());
        let mut ba = PropertyMap::new();
        ba.insert("b", 2i64.into());
        ba.insert("a", 1i64.into());
        let (ab, ba) = (
            ElementProperty::Object(Box::new(ab)),
            ElementProperty::Object(Box::new(ba)),
        );
        assert_eq!(ab, ba);
        assert!(!same(&ab, &ba));
        assert!(same(&ab, &ab.clone()));
        assert!(same(
            &ElementProperty::from("x"),
            &ElementProperty::from("x")
        ));
        assert!(!same(&ElementProperty::from("x"), &ElementProperty::Null));
        let array = ElementProperty::Array(vec![1i64.into(), "y".into()]);
        assert!(same(&array, &array.clone()));
        assert!(!same(
            &array,
            &ElementProperty::Array(vec![1.0f64.into(), "y".into()])
        ));
    }
}
