//! The property-key type shared by every step that names a property.

use std::fmt;

use crate::CompactString;
use crate::value::ArgStrings;
use crate::value::json_path::JsonPath;

/// A property key: either a literal property name or a [`JsonPath`] into a nested property value.
///
/// A plain string is always a literal name (`"a.b"` is a property called `a.b`); a path is only
/// created from an explicit [`JsonPath`], never by sniffing a string. `Clone` is cheap (a
/// `CompactString` or an `Arc`). `Display` is the name, or the canonical path text (`$.a.b[1]`),
/// which is the key used for the key in result maps.
///
/// Public because step signatures take `impl Into<PropKey>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PropKey {
    /// A literal property name.
    Name(CompactString),
    /// A singular JSONPath into a stored property value.
    Path(JsonPath),
}

impl PropKey {
    /// The literal name, or `None` for a path.
    pub fn as_name(&self) -> Option<&str> {
        match self {
            PropKey::Name(n) => Some(n.as_str()),
            PropKey::Path(_) => None,
        }
    }

    /// The top-level property the key reads or writes: the name, or a path's first member
    /// name (`None` for a path starting with an index).
    pub(crate) fn top_level(&self) -> Option<&str> {
        match self {
            PropKey::Name(n) => Some(n.as_str()),
            PropKey::Path(path) => match path.segments().first() {
                Some(crate::value::json_path::JsonPathSegment::Key(key)) => Some(key.as_str()),
                _ => None,
            },
        }
    }

    /// The path, or `None` for a literal name.
    pub fn as_path(&self) -> Option<&JsonPath> {
        match self {
            PropKey::Path(p) => Some(p),
            PropKey::Name(_) => None,
        }
    }
}

impl fmt::Display for PropKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PropKey::Name(n) => f.write_str(n),
            PropKey::Path(p) => fmt::Display::fmt(p, f),
        }
    }
}

impl From<&str> for PropKey {
    fn from(s: &str) -> Self {
        PropKey::Name(CompactString::new(s))
    }
}

impl From<String> for PropKey {
    fn from(s: String) -> Self {
        PropKey::Name(CompactString::from(s))
    }
}

impl From<&String> for PropKey {
    fn from(s: &String) -> Self {
        PropKey::Name(CompactString::new(s))
    }
}

impl From<CompactString> for PropKey {
    fn from(s: CompactString) -> Self {
        PropKey::Name(s)
    }
}

impl From<JsonPath> for PropKey {
    fn from(p: JsonPath) -> Self {
        PropKey::Path(p)
    }
}

impl From<&JsonPath> for PropKey {
    fn from(p: &JsonPath) -> Self {
        PropKey::Path(p.clone())
    }
}

/// What `has_traversal(key, traversal)` runs its traversal on: a property key (a name or a
/// [`JsonPath`], everything that converts to a [`PropKey`]) or the token
/// [`RecordKey::Id`](crate::RecordKey::Id) / [`RecordKey::Label`](crate::RecordKey::Label)
/// (TinkerPop's `has(T.id, traversal)` / `has(T.label, traversal)`). A
/// [`RecordKey::Name`](crate::RecordKey::Name) is the property of that name; any other token
/// fails when the traversal runs.
///
/// Public because `has_traversal` takes `impl Into<HasKey>`; it is built only through its
/// `From` conversions.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HasKey {
    /// A property key.
    Property(PropKey),
    /// A token key (`T.id`, `T.label`).
    Token(crate::RecordKey),
}

impl<T: Into<PropKey>> From<T> for HasKey {
    fn from(key: T) -> Self {
        HasKey::Property(key.into())
    }
}

impl From<crate::RecordKey> for HasKey {
    fn from(key: crate::RecordKey) -> Self {
        match key {
            crate::RecordKey::Name(name) => HasKey::Property(PropKey::Name(name)),
            token => HasKey::Token(token),
        }
    }
}

/// The property-key arguments of `values(..)`/`properties(..)`: none, one key, or a list.
///
/// Anything that converts to a list of literal names (`None`, `"name"`, `vec!["a", "b"]`, ...) keeps working
/// unchanged and means literal names; a [`JsonPath`] or [`PropKey`] (or a `Vec` of them) adds
/// path keys. A path is never created from a string. Only `None` is "no key"; an
/// [`crate::ElementProperty`] that names nothing (null, a map) is an empty list, so
/// `values(ElementProperty::Null)` yields nothing.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum PropKeys {
    /// No key: every property.
    None,
    /// One key.
    One(PropKey),
    /// Several keys, emitted in the given order.
    List(Vec<PropKey>),
}

impl PropKeys {
    /// The literal names when every key is a plain name (`Ok(None)` for no key at all), otherwise
    /// the keys themselves. Plain-name steps keep their exact existing implementation.
    pub(crate) fn into_names(self) -> Result<Option<Vec<String>>, Vec<PropKey>> {
        let keys = match self {
            PropKeys::None => return Ok(None),
            PropKeys::One(key) => vec![key],
            PropKeys::List(keys) => keys,
        };
        if keys.iter().all(|k| matches!(k, PropKey::Name(_))) {
            Ok(Some(keys.into_iter().map(|k| k.to_string()).collect()))
        } else {
            Err(keys)
        }
    }
}

impl<T: Into<ArgStrings>> From<T> for PropKeys {
    fn from(value: T) -> Self {
        match value.into() {
            ArgStrings::String(s) => PropKeys::One(PropKey::from(s)),
            ArgStrings::List(list) => PropKeys::List(list.into_iter().map(PropKey::from).collect()),
            ArgStrings::None => PropKeys::None,
        }
    }
}

impl From<JsonPath> for PropKeys {
    fn from(p: JsonPath) -> Self {
        PropKeys::One(PropKey::Path(p))
    }
}

impl From<&JsonPath> for PropKeys {
    fn from(p: &JsonPath) -> Self {
        PropKeys::One(PropKey::from(p))
    }
}

impl From<PropKey> for PropKeys {
    fn from(k: PropKey) -> Self {
        PropKeys::One(k)
    }
}

impl From<Vec<PropKey>> for PropKeys {
    fn from(keys: Vec<PropKey>) -> Self {
        PropKeys::List(keys)
    }
}

impl From<Vec<JsonPath>> for PropKeys {
    fn from(keys: Vec<JsonPath>) -> Self {
        PropKeys::List(keys.into_iter().map(PropKey::Path).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn path(s: &str) -> JsonPath {
        s.parse().unwrap()
    }

    #[test]
    fn from_conversions() {
        assert_eq!(PropKey::from("a"), PropKey::Name("a".into()));
        assert_eq!(PropKey::from(String::from("a")), PropKey::from("a"));
        assert_eq!(PropKey::from(&String::from("a")), PropKey::from("a"));
        assert_eq!(PropKey::from(CompactString::new("a")), PropKey::from("a"));
        let p = path("a.b[1]");
        assert_eq!(PropKey::from(&p), PropKey::from(p.clone()));
        assert!(matches!(PropKey::from(p), PropKey::Path(_)));
    }

    #[test]
    fn plain_string_with_dots_stays_a_name() {
        let k = PropKey::from("a.b[1]");
        assert_eq!(k.as_name(), Some("a.b[1]"));
        assert!(k.as_path().is_none());
    }

    #[test]
    fn display_name_and_canonical_path() {
        assert_eq!(PropKey::from("a.b").to_string(), "a.b");
        assert_eq!(PropKey::from(path("a.b[1].c")).to_string(), "$.a.b[1].c");
        assert_eq!(
            PropKey::from(path("$['x y'][-1]")).to_string(),
            "$['x y'][-1]"
        );
    }

    #[test]
    fn name_and_path_are_distinct_keys() {
        let name = PropKey::from("$.a");
        let p = PropKey::from(path("a"));
        assert_ne!(name, p);
        assert_eq!(p, PropKey::from(path("$.a")));
        let set: HashSet<PropKey> = [name, p.clone(), PropKey::from(path("$.a"))]
            .into_iter()
            .collect();
        assert_eq!(set.len(), 2);
        assert_eq!(p.as_path().map(|x| x.to_string()), Some("$.a".to_string()));
    }

    #[test]
    fn clone_of_path_shares_segments() {
        let p = PropKey::from(path("a.b"));
        let q = p.clone();
        match (&p, &q) {
            (PropKey::Path(a), PropKey::Path(b)) => assert!(std::sync::Arc::ptr_eq(&a.0, &b.0)),
            _ => panic!("expected paths"),
        }
    }
}
