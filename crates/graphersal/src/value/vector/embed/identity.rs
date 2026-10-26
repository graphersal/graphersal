//! The model identity rule of Model2Vec models (book page "Embeddings"; format spec 4.5): the
//! one place where the text hashed for `model2vec/<repo>@<12 hex>` is built. Shared by the
//! loader (`model2vec.rs`, which hashes the files it reads) and the models a Store keeps
//! (`persist/store/models.rs`, which checks a stored model against its manifest without the
//! `model2vec` feature, also on wasm32).

use super::sha256::{Sha256, hex};

/// The roles of the files the identity covers, in hashing order.
pub(crate) const IDENTITY_ROLES: [&str; 3] = ["config", "tokenizer", "model"];

/// The family prefix of a Model2Vec model identity and of its store directory name.
pub(crate) const FAMILY: &str = "model2vec";

/// The full SHA-256 hex digest of the identity text `model2vec-identity/1\n` followed by one
/// line `<role> <size> <sha256 hex>\n` per file, for the roles `config`, `tokenizer` and
/// `model` in that order (`files` in that order: size and SHA-256 hex of each).
pub(crate) fn identity_digest(files: [(u64, &str); 3]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"model2vec-identity/1\n");
    for (role, (size, sha256)) in IDENTITY_ROLES.iter().zip(files) {
        hash.update(format!("{role} {size} {sha256}\n").as_bytes());
    }
    hex(&hash.finish())
}

/// The identity text of a model: `model2vec/<repo>@<12 hex>`, or `model2vec@<12 hex>` without a
/// repository. `digest` is [`identity_digest`].
pub(crate) fn identity_text(repo: Option<&str>, digest: &str) -> String {
    let short = &digest[..digest.len().min(12)];
    match repo {
        Some(repo) => format!("{FAMILY}/{repo}@{short}"),
        None => format!("{FAMILY}@{short}"),
    }
}

/// The parts of a Model2Vec identity: the repository (when it names one), the 12 hex digits and
/// the normalization override (`?normalize=true|false`), or `None` when `identity` is not one.
#[cfg_attr(not(feature = "persist"), allow(dead_code))]
pub(crate) fn parse_identity(identity: &str) -> Option<ParsedIdentity<'_>> {
    let (base, normalize) = match identity.split_once('?') {
        Some((base, "normalize=true")) => (base, Some(true)),
        Some((base, "normalize=false")) => (base, Some(false)),
        Some(_) => return None,
        None => (identity, None),
    };
    let rest = base.strip_prefix(FAMILY)?;
    let (repo, short) = rest.rsplit_once('@')?;
    let repo = match repo {
        "" => None,
        _ => Some(repo.strip_prefix('/')?).filter(|r| is_repo_name(r)),
    };
    if repo.is_none() && !rest.starts_with('@') {
        return None;
    }
    let hex_ok = short.len() == 12
        && short
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    hex_ok.then_some(ParsedIdentity {
        repo,
        short,
        normalize,
    })
}

/// A parsed Model2Vec identity ([`parse_identity`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(feature = "persist"), allow(dead_code))]
pub(crate) struct ParsedIdentity<'a> {
    /// The repository, when the identity names one.
    pub(crate) repo: Option<&'a str>,
    /// The 12 hex digits.
    pub(crate) short: &'a str,
    /// The normalization override.
    pub(crate) normalize: Option<bool>,
}

/// Whether `repo` is a Hugging Face repository name: `name` or `owner/name`, each part of
/// ASCII letters, digits, `.`, `_` and `-`, not starting with `.`, at most 96 bytes.
pub(crate) fn is_repo_name(repo: &str) -> bool {
    let part_ok = |part: &str| {
        !part.is_empty()
            && part.len() <= 96
            && !part.starts_with('.')
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    let mut parts = repo.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(name), None, None) => part_ok(name),
        (Some(owner), Some(name), None) => part_ok(owner) && part_ok(name),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_parse() {
        let p = parse_identity("model2vec/minishlab/potion-base-2M@0123456789ab").unwrap();
        assert_eq!(p.repo, Some("minishlab/potion-base-2M"));
        assert_eq!(p.short, "0123456789ab");
        assert_eq!(p.normalize, None);
        let p = parse_identity("model2vec@0123456789ab?normalize=false").unwrap();
        assert_eq!((p.repo, p.normalize), (None, Some(false)));
        let p = parse_identity("model2vec/a@0123456789ab").unwrap();
        assert_eq!(p.repo, Some("a"));
        for bad in [
            "",
            "hash-ngram/1",
            "model2vec/@0123456789ab",
            "model2vec@0123456789AB",
            "model2vec@0123456789a",
            "model2vec/a/b/c@0123456789ab",
            "model2vecx@0123456789ab",
            "model2vec@0123456789ab?x=1",
            "model2vec/../x@0123456789ab",
        ] {
            assert!(parse_identity(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn the_identity_text_takes_twelve_digits() {
        let digest = identity_digest([(1, "a"), (2, "b"), (3, "c")]);
        assert_eq!(digest.len(), 64);
        assert_eq!(
            identity_text(Some("o/n"), &digest),
            format!("model2vec/o/n@{}", &digest[..12])
        );
    }
}
