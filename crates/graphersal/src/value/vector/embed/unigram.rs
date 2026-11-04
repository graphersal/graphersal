//! The compact Unigram model of a Model2Vec tokenizer (feature `model2vec`, native only;
//! `value/CLAUDE.md`, section Model2Vec).
//!
//! `tokenizers`' own `Unigram` keeps its vocabulary three times over in small heap objects: a
//! `Vec<(String, f64)>`, an `AHashMap<String, u32>` and a byte trie of one `AHashMap` per node.
//! For the 500 353 tokens of `potion-multilingual-128M` (an 18.6 MB `tokenizer.json`) that is
//! 442 MiB. [`CompactUnigram`] holds the same vocabulary in a few flat arrays (about 25 MiB for
//! that model) and implements `tokenizers`' [`Model`] trait, so it runs inside a
//! `tokenizers::TokenizerImpl` with the normalizer, pre-tokenizer, added tokens, truncation,
//! padding and post-processor the `tokenizer.json` declares, deserialized by `tokenizers` itself.
//! Only the model is ours; its `tokenize` is a transcription of `tokenizers` 0.21's
//! `Unigram::tokenize` and `Unigram::encode_optimized` (the Viterbi search of SentencePiece), so
//! the token ids are the same: the same candidates in the same order, the same `f64` sums, the
//! same strict comparison for ties, the same fusing of unknown pieces and byte fallback.
//!
//! The layout (all built at load time from the `tokenizer.json`, nothing stored):
//!
//! - the vocabulary in id order: the token bytes concatenated (`text`, `text_end[id]` the end of
//!   token `id`) and the scores (`scores[id]`, `f64` as the file has them);
//! - a byte trie over the distinct tokens, its nodes in breadth-first order so the children of a
//!   node are consecutive: `child_start[n]..child_start[n + 1]` are the children of node `n`,
//!   `labels[c]` the byte leading to child `c` (ascending among siblings), `tokens[n]` the id + 1
//!   of the token ending at `n` (0: none; of duplicate tokens the LAST id, as `tokenizers`'
//!   map keeps it); the root's children also in a 256-entry table.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use tokenizers::models::unigram::UnigramTrainer;
use tokenizers::{Model, Token};

/// The score penalty of an unknown piece below the lowest score (`tokenizers`' `K_UNK_PENALTY`).
const UNK_PENALTY: f64 = 10.0;

/// The error message of a model without the `"type": "Unigram"` tag: the caller falls back to
/// `tokenizers`' own model.
pub(crate) const NOT_UNIGRAM: &str = "not a tagged Unigram model";

/// The siblings searched linearly (more: a binary search).
const LINEAR_CHILDREN: usize = 8;

/// A Unigram vocabulary in flat arrays (module docs), a `tokenizers` [`Model`].
pub(crate) struct CompactUnigram {
    child_start: Vec<u32>,
    labels: Vec<u8>,
    tokens: Vec<u32>,
    root: Box<[u32; 256]>,
    scores: Vec<f64>,
    text: Vec<u8>,
    text_end: Vec<u32>,
    min_score: f64,
    unk_id: Option<usize>,
    byte_fallback: bool,
    median_token_length: usize,
}

impl fmt::Debug for CompactUnigram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompactUnigram")
            .field("vocab", &self.scores.len())
            .field("nodes", &self.labels.len())
            .field("unk_id", &self.unk_id)
            .field("byte_fallback", &self.byte_fallback)
            .finish()
    }
}

/// One position of the best-path search (`tokenizers`' `BestPathNode`).
#[derive(Clone, Copy)]
struct BestPath {
    id: usize,
    score: f64,
    /// Where the last piece of the best path ending here starts; `None`: not reached.
    starts_at: Option<usize>,
}

impl CompactUnigram {
    /// The unknown token's id, if the model has one.
    pub(crate) fn unk_id(&self) -> Option<usize> {
        self.unk_id
    }

    /// The median byte length of the distinct tokens (`model2vec-rs`' `median_token_length`:
    /// the length at index `len / 2` of the sorted lengths, 1 for an empty vocabulary).
    pub(crate) fn median_token_length(&self) -> usize {
        self.median_token_length
    }

    /// The ids the model can give: of every distinct token its id (the last of duplicates).
    pub(crate) fn distinct_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.tokens.iter().filter(|t| **t != 0).map(|t| t - 1)
    }

    /// The score of the token `id` (tests compare it with `tokenizers`').
    #[cfg(test)]
    pub(crate) fn score(&self, id: usize) -> Option<f64> {
        self.scores.get(id).copied()
    }

    /// The bytes the model holds on the heap.
    pub(crate) fn heap_bytes(&self) -> u64 {
        (self.child_start.capacity() * 4
            + self.labels.capacity()
            + self.tokens.capacity() * 4
            + std::mem::size_of::<[u32; 256]>()
            + self.scores.capacity() * 8
            + self.text.capacity()
            + self.text_end.capacity() * 4) as u64
    }

    /// The token `id` (any id of the vocabulary, duplicates included).
    fn token(&self, id: usize) -> Option<&str> {
        let end = *self.text_end.get(id)? as usize;
        let start = match id.checked_sub(1) {
            Some(prev) => *self.text_end.get(prev)? as usize,
            None => 0,
        };
        std::str::from_utf8(self.text.get(start..end)?).ok()
    }

    /// The child of `node` along `byte`.
    #[inline]
    fn child(&self, node: usize, byte: u8) -> Option<usize> {
        if node == 0 {
            let child = self.root[usize::from(byte)];
            return (child != 0).then_some(child as usize);
        }
        let lo = *self.child_start.get(node)? as usize;
        let hi = *self.child_start.get(node + 1)? as usize;
        let labels = self.labels.get(lo..hi)?;
        let at = if labels.len() <= LINEAR_CHILDREN {
            labels.iter().position(|label| *label == byte)?
        } else {
            labels.binary_search(&byte).ok()?
        };
        Some(lo + at)
    }

    /// The id of the token `bytes` (the last id of duplicates), as `tokenizers`' map gives it.
    fn find(&self, bytes: &[u8]) -> Option<u32> {
        let mut node = 0;
        for byte in bytes {
            node = self.child(node, *byte)?;
        }
        match self.tokens.get(node) {
            Some(token) if *token != 0 => Some(token - 1),
            _ => None,
        }
    }

    /// The best segmentation of `sentence` as byte ranges, unknown pieces fused
    /// (`Unigram::encode_optimized`).
    fn encode(&self, sentence: &str) -> tokenizers::Result<Vec<(usize, usize)>> {
        if sentence.is_empty() {
            return Ok(Vec::new());
        }
        let bytes = sentence.as_bytes();
        let size = bytes.len();
        let unk_score = self.min_score - UNK_PENALTY;
        let mut best = vec![
            BestPath {
                id: 0,
                score: 0.0,
                starts_at: None,
            };
            size + 1
        ];
        let mut starts_at = 0;
        while starts_at < size {
            let till_here = best[starts_at].score;
            let mut has_single_node = false;
            let mblen = sentence
                .get(starts_at..)
                .and_then(|rest| rest.chars().next())
                .map_or(1, char::len_utf8);
            // The trie's common-prefix search: shorter tokens first, as `tokenizers` yields them.
            let mut node = 0;
            let mut key_pos = starts_at;
            while key_pos < size {
                let Some(next) = self.child(node, bytes[key_pos]) else {
                    break;
                };
                node = next;
                key_pos += 1;
                let token = self.tokens.get(node).copied().unwrap_or(0);
                if token == 0 {
                    continue;
                }
                let id = (token - 1) as usize;
                let score = self.scores.get(id).copied().unwrap_or(0.0);
                let candidate = score + till_here;
                let target = &mut best[key_pos];
                if target.starts_at.is_none() || candidate > target.score {
                    target.score = candidate;
                    target.starts_at = Some(starts_at);
                    target.id = id;
                }
                if !has_single_node && key_pos - starts_at == mblen {
                    has_single_node = true;
                }
            }
            if !has_single_node {
                let target = &mut best[starts_at + mblen];
                let candidate = unk_score + till_here;
                if target.starts_at.is_none() || candidate > target.score {
                    target.score = candidate;
                    target.starts_at = Some(starts_at);
                    target.id = self.unk_id.ok_or_else(missing_unk_id)?;
                }
            }
            starts_at += mblen;
        }
        let mut ends_at = size;
        let mut results: Vec<(usize, usize)> = Vec::new();
        // The unknown pieces met since the last known one (fused into one piece).
        let mut unknown: Option<(usize, usize)> = None;
        while ends_at > 0 {
            let node = best[ends_at];
            let Some(starts_at) = node.starts_at else {
                return Err(missing_unk_id());
            };
            if Some(node.id) == self.unk_id {
                unknown = Some((starts_at, unknown.map_or(ends_at, |(_, end)| end)));
            } else {
                if let Some(piece) = unknown.take() {
                    results.push(piece);
                }
                results.push((starts_at, ends_at));
            }
            ends_at = starts_at;
        }
        if let Some(piece) = unknown {
            results.push(piece);
        }
        results.reverse();
        Ok(results)
    }
}

fn missing_unk_id() -> tokenizers::Error {
    "Encountered an unknown token but `unk_id` is missing".into()
}

impl Model for CompactUnigram {
    /// Never used: the compact model is not trained (`tokenizers` requires a trainer type).
    type Trainer = UnigramTrainer;

    /// `Unigram::tokenize`: the pieces of [`Self::encode`], each with its id from the vocabulary
    /// (a fused unknown piece too), else its bytes as `<0xXX>` tokens when the model has byte
    /// fallback and all of them, else the unknown id.
    fn tokenize(&self, sentence: &str) -> tokenizers::Result<Vec<Token>> {
        let pieces = self.encode(sentence)?;
        let mut offset = 0;
        let mut tokens = Vec::with_capacity(pieces.len());
        for (start, end) in pieces {
            let string = sentence
                .get(start..end)
                .ok_or("a piece is not on a character boundary")?;
            let len = string.len();
            let offsets = (offset, offset + len);
            let id = match self.find(string.as_bytes()) {
                Some(id) => id,
                None => {
                    if self.byte_fallback {
                        let byte_tokens: Option<Vec<Token>> = string
                            .bytes()
                            .map(|byte| {
                                let byte_string = format!("<0x{byte:02X}>");
                                self.find(byte_string.as_bytes())
                                    .map(|id| Token::new(id, byte_string, (offset, offset + len)))
                            })
                            .collect();
                        if let Some(byte_tokens) = byte_tokens {
                            tokens.extend(byte_tokens);
                            offset += len;
                            continue;
                        }
                    }
                    self.unk_id.ok_or_else(missing_unk_id)? as u32
                }
            };
            offset += len;
            tokens.push(Token::new(id, string.to_string(), offsets));
        }
        Ok(tokens)
    }

    fn token_to_id(&self, token: &str) -> Option<u32> {
        self.find(token.as_bytes())
    }

    fn id_to_token(&self, id: u32) -> Option<String> {
        self.token(id as usize).map(str::to_string)
    }

    fn get_vocab(&self) -> HashMap<String, u32> {
        let mut vocab = HashMap::with_capacity(self.scores.len());
        for id in 0..self.scores.len() {
            if let Some(token) = self.token(id) {
                vocab.insert(token.to_string(), id as u32);
            }
        }
        vocab
    }

    fn get_vocab_size(&self) -> usize {
        self.scores.len()
    }

    fn save(&self, _folder: &Path, _prefix: Option<&str>) -> tokenizers::Result<Vec<PathBuf>> {
        Err("the compact Unigram model is not saved".into())
    }

    fn get_trainer(&self) -> UnigramTrainer {
        UnigramTrainer::default()
    }
}

/// The vocabulary while it is read, in id order.
struct Builder {
    text: Vec<u8>,
    text_end: Vec<u32>,
    scores: Vec<f64>,
    min_score: f64,
}

impl Builder {
    fn new() -> Self {
        Builder {
            text: Vec::new(),
            text_end: Vec::new(),
            scores: Vec::new(),
            min_score: f64::INFINITY,
        }
    }

    fn push(&mut self, token: &str, score: f64) -> Result<(), String> {
        self.text.extend_from_slice(token.as_bytes());
        let end = u32::try_from(self.text.len())
            .map_err(|_| "the vocabulary is too large for the compact model".to_string())?;
        // Ids + 1 are kept in a u32.
        if self.scores.len() >= u32::MAX as usize - 1 {
            return Err("the vocabulary is too large for the compact model".to_string());
        }
        self.text_end.push(end);
        self.scores.push(score);
        if score < self.min_score {
            self.min_score = score;
        }
        Ok(())
    }

    fn bytes(&self, id: u32) -> &[u8] {
        let id = id as usize;
        let end = self.text_end.get(id).copied().unwrap_or(0) as usize;
        let start = match id.checked_sub(1) {
            Some(prev) => self.text_end.get(prev).copied().unwrap_or(0) as usize,
            None => 0,
        };
        self.text.get(start..end).unwrap_or(&[])
    }

    /// The model, with `Unigram::from`'s checks.
    fn build(self, unk_id: Option<usize>, byte_fallback: bool) -> Result<CompactUnigram, String> {
        let n = self.scores.len();
        if let Some(unk_id) = unk_id {
            if n == 0 {
                return Err("The vocabulary is empty but at least <unk> is needed".to_string());
            }
            if unk_id >= n {
                return Err("The `unk_id` is larger than vocabulary size".to_string());
            }
        }
        // The distinct tokens in byte order, each with its LAST id.
        let mut order: Vec<u32> = (0..n as u32).collect();
        order.sort_unstable_by(|a, b| self.bytes(*a).cmp(self.bytes(*b)).then(a.cmp(b)));
        let mut distinct: Vec<u32> = Vec::with_capacity(n);
        for id in order {
            match distinct.last_mut() {
                Some(last) if self.bytes(*last) == self.bytes(id) => *last = id,
                _ => distinct.push(id),
            }
        }
        let mut lengths: Vec<usize> = distinct.iter().map(|id| self.bytes(*id).len()).collect();
        lengths.sort_unstable();
        let median_token_length = lengths.get(lengths.len() / 2).copied().unwrap_or(1);
        drop(lengths);
        // The trie, breadth first: node `n` covers `distinct[range.0..range.1]`, which share
        // their first `depth` bytes.
        let mut child_start: Vec<u32> = Vec::new();
        let mut labels: Vec<u8> = vec![0];
        let mut tokens: Vec<u32> = vec![0];
        let mut ranges: Vec<(u32, u32, u32)> = vec![(0, distinct.len() as u32, 0)];
        let mut node = 0;
        while node < labels.len() {
            let (lo, hi, depth) = ranges[node];
            let (mut lo, hi, depth) = (lo as usize, hi as usize, depth as usize);
            child_start.push(
                u32::try_from(labels.len())
                    .map_err(|_| "the vocabulary is too large for the compact model")?,
            );
            if lo < hi && self.bytes(distinct[lo]).len() == depth {
                tokens[node] = distinct[lo] + 1;
                lo += 1;
            }
            while lo < hi {
                let byte = self.bytes(distinct[lo]).get(depth).copied().unwrap_or(0);
                let mut end = lo + 1;
                while end < hi && self.bytes(distinct[end]).get(depth) == Some(&byte) {
                    end += 1;
                }
                labels.push(byte);
                tokens.push(0);
                ranges.push((lo as u32, end as u32, depth as u32 + 1));
                lo = end;
            }
            node += 1;
        }
        child_start.push(
            u32::try_from(labels.len())
                .map_err(|_| "the vocabulary is too large for the compact model")?,
        );
        drop(ranges);
        drop(distinct);
        let mut root = Box::new([0u32; 256]);
        let (first, last) = (child_start[0] as usize, child_start[1] as usize);
        for child in first..last {
            root[usize::from(labels[child])] = child as u32;
        }
        child_start.shrink_to_fit();
        labels.shrink_to_fit();
        tokens.shrink_to_fit();
        let Builder {
            mut text,
            mut text_end,
            mut scores,
            min_score,
        } = self;
        text.shrink_to_fit();
        text_end.shrink_to_fit();
        scores.shrink_to_fit();
        Ok(CompactUnigram {
            child_start,
            labels,
            tokens,
            root,
            scores,
            text,
            text_end,
            min_score,
            unk_id,
            byte_fallback,
            median_token_length,
        })
    }
}

impl<'de> serde::Deserialize<'de> for CompactUnigram {
    /// The `model` object of a `tokenizer.json` with `"type": "Unigram"` (the fields
    /// `tokenizers` reads: `unk_id`, `vocab`, `byte_fallback`; others ignored as it ignores
    /// them). Without the tag it fails with [`NOT_UNIGRAM`]. The vocabulary streams into the
    /// arrays: no `(String, f64)` list is kept.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(ModelVisitor)
    }
}

struct ModelVisitor;

impl<'de> Visitor<'de> for ModelVisitor {
    type Value = CompactUnigram;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a Unigram model")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<CompactUnigram, A::Error> {
        let mut tagged = false;
        let mut unk_id: Option<usize> = None;
        let mut byte_fallback = false;
        let mut builder: Option<Builder> = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "type" => {
                    let kind: String = map.next_value()?;
                    if kind != "Unigram" {
                        return Err(de::Error::custom(NOT_UNIGRAM));
                    }
                    tagged = true;
                }
                "unk_id" => unk_id = map.next_value()?,
                "byte_fallback" => byte_fallback = map.next_value()?,
                "vocab" => {
                    let mut read = Builder::new();
                    map.next_value_seed(VocabSeed(&mut read))?;
                    builder = Some(read);
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        if !tagged {
            return Err(de::Error::custom(NOT_UNIGRAM));
        }
        let Some(builder) = builder else {
            return Err(de::Error::custom("Missing vocab"));
        };
        builder
            .build(unk_id, byte_fallback)
            .map_err(|err| de::Error::custom(format!("Unable to load vocab {err}")))
    }
}

/// The `vocab` list (`[token, score]` pairs) read into a [`Builder`].
struct VocabSeed<'b>(&'b mut Builder);

impl<'de> DeserializeSeed<'de> for VocabSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for VocabSeed<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a list of [token, score] pairs")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while let Some((token, score)) = seq.next_element::<(String, f64)>()? {
            self.0.push(&token, score).map_err(de::Error::custom)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "unigram_tests.rs"]
mod tests;
