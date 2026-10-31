//! `hash-ngram/1`: the built-in embedder of hashed words and character n-grams (book page
//! "Vectors", section "Embeddings"). The algorithm is FROZEN: any change of the output bits is a
//! new name (`hash-ngram/2`); golden tests (`tests/all/embed_tests.rs`, and the wasm32 parity
//! test of `graphersal-wasm`) pin the bits.
//!
//! For one text:
//!
//! 1. every `char` is lowercased (`char::to_lowercase`, all produced chars kept; no Unicode
//!    normalization);
//! 2. words are the maximal runs of `char::is_alphanumeric` chars, every other char separates;
//! 3. the features of a word `w` are the word itself (bytes `b"W"` + UTF-8 of `w`) and the
//!    char n-grams of `"<" + w + ">"` for n = 3, 4, 5 (bytes `b"N"` + UTF-8 of the n-gram; a
//!    bracketed word shorter than n has no n-gram of that n);
//! 4. a feature is hashed with 64-bit FNV-1a, then the splitmix64 finalizer; the index is the
//!    hash modulo the dimension, the sign `+1` when bit 63 is 0, else `-1`;
//! 5. the signs are summed per index in `i64`;
//! 6. the counts are divided by their L2 norm (sum of squares in `f64` in index order, `sqrt`),
//!    each component rounded to `f32`: only correctly rounded IEEE operations, so every target
//!    gives the same bits;
//! 7. a text without a feature (or whose counts all cancel) is [`EmbedError::NoFeatures`].

use compact_str::CompactString;

use super::{EmbedContext, EmbedError, Embedder, EmbedderInfo};
use crate::value::vector::MAX_VECTOR_DIMENSION;
use crate::value::{Vector, VectorElement, VectorMetric};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// The built-in embedder `hash-ngram/1`: hashed words and character n-grams, no model file, no
/// dependency, the same bits on every target (module docs of this file for the algorithm).
///
/// Lexical similarity only: typos, word forms and shared stems come out close, in any script;
/// meaning does not ("car" and "automobile" are unrelated). Good for fuzzy lookup, entity
/// matching, deduplication and as a baseline before a semantic model.
///
/// [`Embedders::default`](super::Embedders) holds it as `hash-ngram/1` (256 f32 components,
/// metric cosine, 256 texts per call, deterministic, local); the DSL function `embed(text)`
/// computes it outside the engine.
#[derive(Debug, Clone)]
pub(crate) struct HashNgram {
    info: EmbedderInfo,
}

impl Default for HashNgram {
    fn default() -> Self {
        Self::new()
    }
}

impl HashNgram {
    /// The registry name and model identity of the built-in instance.
    pub(crate) const NAME: &'static str = "hash-ngram/1";

    /// The dimension of the built-in instance.
    pub(crate) const DEFAULT_DIMENSION: usize = 256;

    /// The built-in instance: `hash-ngram/1`, 256 f32 components.
    pub(crate) fn new() -> Self {
        HashNgram {
            info: Self::info_of(Self::NAME, Self::DEFAULT_DIMENSION).with_model(Self::NAME),
        }
    }

    /// The same algorithm with `dimension` components (1 to
    /// [`MAX_VECTOR_DIMENSION`](crate::vector::MAX_VECTOR_DIMENSION)) under `name` (tests). Its
    /// model identity is `hash-ngram/1?dim=<dimension>`.
    #[allow(dead_code)]
    pub(crate) fn with_dimension(
        name: impl Into<CompactString>,
        dimension: usize,
    ) -> Result<Self, String> {
        if dimension == 0 || dimension > MAX_VECTOR_DIMENSION {
            return Err(format!(
                "hash-ngram has 1 to {MAX_VECTOR_DIMENSION} components, not {dimension}"
            ));
        }
        let model = format!("{}?dim={dimension}", Self::NAME);
        Ok(HashNgram {
            info: Self::info_of(name.into(), dimension).with_model(model),
        })
    }

    fn info_of(name: impl Into<CompactString>, dimension: usize) -> EmbedderInfo {
        EmbedderInfo::new(name, dimension, VectorElement::F32)
            .with_metric(VectorMetric::Cosine)
            .with_max_batch(256)
            .with_deterministic(true)
            .with_local(true)
    }
}

impl Embedder for HashNgram {
    fn info(&self) -> &EmbedderInfo {
        &self.info
    }

    fn embed(&self, texts: &[&str], ctx: &EmbedContext<'_>) -> Result<Vec<Vector>, EmbedError> {
        let dimension = self.info.dimension;
        let mut out = Vec::with_capacity(texts.len());
        let mut word: Vec<char> = Vec::new();
        for (index, text) in texts.iter().enumerate() {
            ctx.check()?;
            let mut counts = vec![0i64; dimension];
            let mut features = false;
            for c in text.chars().flat_map(char::to_lowercase) {
                if c.is_alphanumeric() {
                    word.push(c);
                } else if !word.is_empty() {
                    add_word(&word, &mut counts);
                    features = true;
                    word.clear();
                }
            }
            if !word.is_empty() {
                add_word(&word, &mut counts);
                features = true;
                word.clear();
            }
            if !features {
                return Err(EmbedError::no_features(index));
            }
            let components = normalize(&counts).ok_or(EmbedError::no_features(index))?;
            out.push(
                Vector::from_f32(components).map_err(|err| EmbedError::failed(err.to_string()))?,
            );
        }
        Ok(out)
    }
}

/// The FNV-1a state fed one byte at a time.
#[derive(Clone, Copy)]
struct Fnv(u64);

impl Fnv {
    #[inline]
    fn byte(&mut self, byte: u8) {
        self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
    }

    #[inline]
    fn char(&mut self, c: char) {
        let mut buffer = [0u8; 4];
        for byte in c.encode_utf8(&mut buffer).as_bytes() {
            self.byte(*byte);
        }
    }

    /// The splitmix64 finalizer of the hash.
    #[inline]
    fn finish(self) -> u64 {
        let mut x = self.0;
        x ^= x >> 30;
        x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x ^= x >> 27;
        x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^= x >> 31;
        x
    }
}

/// Adds the sign of one feature hash.
#[inline]
fn add(hash: u64, counts: &mut [i64]) {
    let index = (hash % counts.len() as u64) as usize;
    if hash >> 63 == 0 {
        counts[index] += 1;
    } else {
        counts[index] -= 1;
    }
}

/// Adds the features of one word (lowercased chars, at least one).
fn add_word(word: &[char], counts: &mut [i64]) {
    let mut whole = Fnv(FNV_OFFSET);
    whole.byte(b'W');
    for c in word {
        whole.char(*c);
    }
    add(whole.finish(), counts);
    // The bracketed form `<` + word + `>`: position 0 is `<`, position len + 1 is `>`.
    let bracketed = word.len() + 2;
    for n in 3..=5usize {
        if bracketed < n {
            continue;
        }
        for start in 0..=bracketed - n {
            let mut gram = Fnv(FNV_OFFSET);
            gram.byte(b'N');
            for position in start..start + n {
                if position == 0 {
                    gram.byte(b'<');
                } else if position == bracketed - 1 {
                    gram.byte(b'>');
                } else {
                    gram.char(word[position - 1]);
                }
            }
            add(gram.finish(), counts);
        }
    }
}

/// The counts divided by their L2 norm, as f32; `None` when every count is zero.
fn normalize(counts: &[i64]) -> Option<Vec<f32>> {
    let mut sum = 0.0f64;
    for count in counts {
        let value = *count as f64;
        sum += value * value;
    }
    if sum == 0.0 {
        return None;
    }
    let norm = sum.sqrt();
    Some(
        counts
            .iter()
            .map(|count| (*count as f64 / norm) as f32)
            .collect(),
    )
}
