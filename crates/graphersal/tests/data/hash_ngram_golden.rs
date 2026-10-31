// The golden vectors of `hash-ngram/1`: each text with the FNV-1a 64 hash of its 256 f32
// components' bits (little endian, in index order). `hash-ngram/1` is frozen: these values never
// change (a change of the algorithm is a new name, `hash-ngram/2`). Shared by
// the unit tests of `crates/graphersal/src/value/vector/embed/hash_ngram.rs` (native) and
// `crates/graphersal-wasm/tests/embed_parity.rs` (wasm32), so both targets give the same bits.
// Included with `include!`, not a module.

/// (text, FNV-1a 64 of the vector's f32 bits).
pub const HASH_NGRAM_GOLDEN: &[(&str, u64)] = &[
    ("graph", 10964353409179732294),
    ("Graph Database", 3101220846043765508),
    ("Привет, мир", 17774893794003775145),
    ("MiXeD CaSe 42", 13419719659570185133),
    ("emoji 🚀 rocket", 13647738931263631933),
    ("a", 10824490952998444657),
    ("ab", 11741615585269472645),
    ("...!!! hello ???", 6748887349568104326),
    ("naïve café", 15766712614119095893),
    ("日本語のテキスト", 10649307462000220949),
    ("The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. \
      Pack my box with five dozen liquor jugs; how vexingly quick daft zebras jump! 0123456789",
     12530827788983755679),
];

/// FNV-1a 64 over the little-endian bits of `components`.
pub fn hash_ngram_bits_hash(components: &[f32]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for component in components {
        for byte in component.to_bits().to_le_bytes() {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}
