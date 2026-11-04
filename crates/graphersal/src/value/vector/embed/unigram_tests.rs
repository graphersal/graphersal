//! The compact Unigram model against `tokenizers`' own `Unigram` (the reference): the same
//! tokens (ids, strings, offsets) for random vocabularies and sentences, with ties, duplicate
//! tokens, unknown characters, byte fallback and models without an unknown token; the same
//! vocabulary answers; the deserialization's checks and its refusal of untagged models. The
//! whole pipeline (normalizer, pre-tokenizer, added tokens) and the vectors: `model2vec_tests.rs`.

use tokenizers::Model;
use tokenizers::models::unigram::Unigram;

use super::*;

/// A small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// The compact model of `vocab`.
fn compact(vocab: &[(String, f64)], unk_id: Option<usize>, byte_fallback: bool) -> CompactUnigram {
    let mut builder = Builder::new();
    for (token, score) in vocab {
        builder.push(token, *score).unwrap();
    }
    builder.build(unk_id, byte_fallback).unwrap()
}

/// Both models of `vocab`.
fn both(
    vocab: &[(String, f64)],
    unk_id: Option<usize>,
    byte_fallback: bool,
) -> (Unigram, CompactUnigram) {
    let reference = Unigram::from(vocab.to_vec(), unk_id, byte_fallback).unwrap();
    (reference, compact(vocab, unk_id, byte_fallback))
}

/// Asserts the same tokens (or both an error) for `sentence`.
fn assert_same_tokens(reference: &Unigram, ours: &CompactUnigram, sentence: &str) {
    let expected = reference.tokenize(sentence);
    let actual = ours.tokenize(sentence);
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => assert_eq!(expected, actual, "sentence {sentence:?}"),
        (Err(expected), Err(actual)) => {
            assert_eq!(expected.to_string(), actual.to_string(), "{sentence:?}")
        }
        (expected, actual) => panic!("{sentence:?}: {expected:?} vs {actual:?}"),
    }
}

/// The characters vocabularies are made of: ASCII, the metaspace, Latin with diacritics, a
/// combining mark, Cyrillic, CJK, an emoji (4 bytes) and its skin-tone modifier, a space.
const ALPHABET: [&str; 14] = [
    "a",
    "b",
    "c",
    "d",
    "\u{2581}",
    "é",
    "\u{301}",
    "ж",
    "д",
    "中",
    "文",
    "😀",
    "\u{1f3fd}",
    " ",
];

/// Characters only sentences have (unknown to every vocabulary).
const UNKNOWN: [&str; 5] = ["x", "й", "🙂", "\u{0}", "\u{ffff}"];

/// A random vocabulary: tokens of 1 to 5 characters, scores from a small set (ties), some
/// duplicates; the unknown token `<unk>` at a random place; with byte fallback, `<0xXX>` tokens
/// for some bytes.
fn random_vocab(rng: &mut Rng, byte_fallback: bool) -> (Vec<(String, f64)>, usize) {
    let scores = [-1.0, -1.5, -2.0, -2.5, -3.0, -0.5, -7.25];
    let size = 5 + rng.below(60);
    let mut vocab: Vec<(String, f64)> = Vec::new();
    for _ in 0..size {
        let chars = 1 + rng.below(5);
        let token: String = (0..chars).map(|_| *rng.pick(&ALPHABET)).collect();
        vocab.push((token, *rng.pick(&scores)));
        if rng.below(10) == 0 {
            // A duplicate with another score: the later id wins, as in `tokenizers`.
            let again = vocab[rng.below(vocab.len())].0.clone();
            vocab.push((again, *rng.pick(&scores)));
        }
    }
    if byte_fallback {
        for byte in [0x78u8, 0xd0, 0xb9, 0xf0, 0x9f, 0x99, 0x82, 0x00] {
            if rng.below(4) != 0 {
                vocab.push((format!("<0x{byte:02X}>"), -9.0));
            }
        }
    }
    let unk_id = rng.below(vocab.len() + 1);
    vocab.insert(unk_id, ("<unk>".to_string(), *rng.pick(&scores)));
    (vocab, unk_id)
}

/// A random sentence over the alphabet, the unknown characters and the tokens of `vocab`.
fn random_sentence(rng: &mut Rng, vocab: &[(String, f64)]) -> String {
    let parts = rng.below(12);
    let mut sentence = String::new();
    for _ in 0..parts {
        match rng.below(4) {
            0 => sentence.push_str(rng.pick(&UNKNOWN)),
            1 => sentence.push_str(&rng.pick(vocab).0),
            _ => sentence.push_str(rng.pick(&ALPHABET)),
        }
    }
    sentence
}

#[test]
fn random_vocabularies_tokenize_as_tokenizers_unigram() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    // How often the rare paths ran: byte-fallback tokens, unknown pieces of several characters.
    let (mut byte_tokens, mut fused) = (0, 0);
    for case in 0..400 {
        let byte_fallback = case % 3 == 0;
        let (vocab, unk_id) = random_vocab(&mut rng, byte_fallback);
        let (reference, ours) = both(&vocab, Some(unk_id), byte_fallback);
        for _ in 0..40 {
            let sentence = random_sentence(&mut rng, &vocab);
            assert_same_tokens(&reference, &ours, &sentence);
            for token in ours.tokenize(&sentence).unwrap() {
                byte_tokens += usize::from(token.value.starts_with("<0x") && token.id != 0);
                fused +=
                    usize::from(token.id as usize == unk_id && token.value.chars().count() > 1);
            }
        }
        for (token, _) in &vocab {
            assert_same_tokens(&reference, &ours, token);
            assert_eq!(reference.token_to_id(token), ours.token_to_id(token));
        }
        assert_same_tokens(&reference, &ours, "");
        assert_eq!(reference.get_vocab_size(), ours.get_vocab_size());
        assert_eq!(reference.get_vocab(), ours.get_vocab());
        for id in 0..vocab.len() as u32 + 2 {
            assert_eq!(reference.id_to_token(id), ours.id_to_token(id));
        }
        assert_eq!(reference.token_to_id("x"), ours.token_to_id("x"));
    }
    assert!(byte_tokens > 100 && fused > 100, "{byte_tokens} {fused}");
}

#[test]
fn a_model_without_an_unknown_token_fails_as_tokenizers_does() {
    let mut rng = Rng(42);
    for _ in 0..200 {
        let (mut vocab, unk_id) = random_vocab(&mut rng, false);
        vocab.remove(unk_id);
        let (reference, ours) = both(&vocab, None, false);
        for _ in 0..20 {
            let sentence = random_sentence(&mut rng, &vocab);
            assert_same_tokens(&reference, &ours, &sentence);
        }
    }
    // An empty vocabulary without an unknown token loads; every character is then unknown.
    let (reference, ours) = both(&[], None, false);
    assert_same_tokens(&reference, &ours, "abc");
    assert_same_tokens(&reference, &ours, "");
}

#[test]
fn ties_keep_the_first_candidate_and_unknown_pieces_fuse() {
    let vocab: Vec<(String, f64)> = [
        ("<unk>", -1.0),
        ("a", -1.0),
        ("b", -1.0),
        ("ab", -2.0),
        ("abc", -3.0),
        ("c", -1.0),
        // Matched only as a whole: its first character alone is unknown.
        ("xy", -100.0),
        ("", -0.5),
    ]
    .iter()
    .map(|(t, s)| (t.to_string(), *s))
    .collect();
    let (reference, ours) = both(&vocab, Some(0), false);
    for sentence in [
        "ab", "abc", "abcab", "xyz", "zzab", "xy", "x", "ababab", "aé", "éé", "<unk>", "a<unk>b",
    ] {
        assert_same_tokens(&reference, &ours, sentence);
    }
    let tokens = ours.tokenize("zzab").unwrap();
    assert_eq!(tokens[0].value, "zz");
    assert_eq!(tokens[0].id, 0);
    // The empty token is in the vocabulary but never matched.
    assert_eq!(ours.token_to_id(""), Some(7));
}

#[test]
fn the_compact_model_reports_its_size_and_median() {
    let vocab: Vec<(String, f64)> = ["<unk>", "a", "bb", "ccc", "dddd", "bb"]
        .iter()
        .map(|t| (t.to_string(), -1.0))
        .collect();
    let ours = compact(&vocab, Some(0), false);
    // Distinct lengths sorted: 1, 2, 3, 4, 5 -> index 2.
    assert_eq!(ours.median_token_length(), 3);
    assert_eq!(ours.unk_id(), Some(0));
    let mut ids: Vec<u32> = ours.distinct_ids().collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![0, 1, 3, 4, 5]);
    assert!(ours.heap_bytes() > 0);
    assert_eq!(compact(&[], None, false).median_token_length(), 1);
}

#[test]
fn deserialization_reads_the_tagged_model_and_refuses_others() {
    let json = r#"{"type": "Unigram", "unk_id": 0, "extra": [1, {"x": 2}],
                   "vocab": [["<unk>", 0.0], ["a", -0.5], ["é\n", -1.25]],
                   "byte_fallback": false}"#;
    let ours: CompactUnigram = serde_json::from_str(json).unwrap();
    assert_eq!(ours.get_vocab_size(), 3);
    assert_eq!(ours.token_to_id("é\n"), Some(2));
    assert_eq!(ours.scores, vec![0.0, -0.5, -1.25]);
    let refused = |json: &str| match serde_json::from_str::<CompactUnigram>(json) {
        Ok(_) => panic!("accepted {json}"),
        Err(err) => err.to_string(),
    };
    assert!(refused(r#"{"unk_id": 0, "vocab": [["<unk>", 0.0]]}"#).contains(NOT_UNIGRAM));
    assert!(refused(r#"{"type": "BPE", "vocab": {}, "merges": []}"#).contains(NOT_UNIGRAM));
    assert!(refused(r#"{"type": "Unigram", "unk_id": 0}"#).contains("Missing vocab"));
    assert!(
        refused(r#"{"type": "Unigram", "unk_id": 3, "vocab": [["a", 0.0]]}"#)
            .contains("larger than vocabulary size")
    );
    assert!(refused(r#"{"type": "Unigram", "unk_id": 0, "vocab": []}"#).contains("empty"));
    let extra = refused(r#"{"type": "Unigram", "vocab": [["a", 0.0, 1]]}"#);
    assert!(
        extra.contains("trailing") || extra.contains("length"),
        "{extra}"
    );
    // The same answers as `tokenizers`' deserialization of the same model.
    let reference: Unigram = serde_json::from_str(
        r#"{"unk_id": 0, "vocab": [["<unk>", 0.0], ["a", -0.5], ["é\n", -1.25]]}"#,
    )
    .unwrap();
    assert_eq!(reference.get_vocab(), ours.get_vocab());
}

#[test]
fn a_large_vocabulary_builds_a_compact_trie() {
    let mut rng = Rng(7);
    let mut vocab: Vec<(String, f64)> = vec![("<unk>".to_string(), -20.0)];
    for i in 0..20_000 {
        let chars = 1 + rng.below(8);
        let token: String = (0..chars).map(|_| *rng.pick(&ALPHABET)).collect();
        vocab.push((token, -((i % 97) as f64) / 7.0));
    }
    let (reference, ours) = both(&vocab, Some(0), false);
    for _ in 0..300 {
        let sentence = random_sentence(&mut rng, &vocab);
        assert_same_tokens(&reference, &ours, &sentence);
    }
    // A long sentence: one best path over thousands of positions.
    let long: String = (0..3000).map(|_| *rng.pick(&ALPHABET)).collect();
    assert_same_tokens(&reference, &ours, &long);
}
