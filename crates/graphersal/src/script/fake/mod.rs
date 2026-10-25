//! `Fake(seed)`: a seeded generator of test data for scripts (English names, addresses,
//! companies, words) and of the random numbers a script needs to build a graph's topology.
//!
//! **Reproducibility contract.** The same seed and the same sequence of calls give the same
//! values on every platform (native and wasm32) and in every run. The generator is our own
//! (SplitMix64, [`Rng`]), not `rand`'s `StdRng`, whose algorithm may change between `rand`
//! versions; the word lists (`data.rs`) and the way each method draws are part of the contract
//! too, so changing either changes what a seed produces. The golden test in `tests.rs` pins it.
//!
//! **A handle, not a value.** A `Fake` value is shared by its copies (`let b = a;`, a function
//! argument): they draw from one stream. A helper `fn person(f) { f.first_name() }` therefore
//! advances the caller's generator, which is what a script means; with value semantics every
//! call would return the same name. [`ScriptFake::fork`] gives an independent stream derived
//! from the seed and a name, so drawing more values in one part of a script does not shift the
//! values of another.

mod data;
#[cfg(test)]
mod tests;

use super::register::Registry;
use super::tokens::{ScriptUuid, script_uuid_v4};
use crate::error::ScriptError;
use rhai::{Array, Dynamic, EvalAltResult, INT};
use std::sync::{Arc, Mutex, PoisonError};

use data::*;

/// The SplitMix64 generator (Steele, Lea, Flood 2014): one 64-bit state, a fixed increment and
/// a finalizer. Small, fast, statistically fine for test data, and fully specified here.
#[derive(Debug, Clone)]
struct Rng {
    state: u64,
}

/// The SplitMix64 increment (the golden ratio in 64 bits).
const GOLDEN_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// The SplitMix64 finalizer: a bijective mix of 64 bits.
fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// FNV-1a over the bytes of a fork name: a hash that never changes (unlike `std`'s hasher).
fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xCBF2_9CE4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3)
    })
}

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(GOLDEN_GAMMA);
        mix64(self.state)
    }

    /// A uniform value in `0..n`, without modulo bias (Lemire's multiply-and-reject); `0` for
    /// `n == 0`, which no caller passes.
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        let threshold = n.wrapping_neg() % n;
        loop {
            let product = u128::from(self.next_u64()) * u128::from(n);
            if (product as u64) >= threshold {
                return (product >> 64) as u64;
            }
        }
    }

    /// A uniform index into a non-empty slice of `len` items.
    fn index(&mut self, len: usize) -> usize {
        self.below(len as u64) as usize
    }

    /// A uniform item of a non-empty list (the default value of an empty one).
    fn pick<T: Copy + Default>(&mut self, items: &[T]) -> T {
        let index = self.index(items.len().max(1));
        items.get(index).copied().unwrap_or_default()
    }

    /// A uniform float in `[0, 1)` from the top 53 bits.
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// A uniform integer in `lo..=hi` (`lo <= hi`).
    fn int(&mut self, lo: i64, hi: i64) -> i64 {
        let span = (i128::from(hi) - i128::from(lo) + 1) as u128;
        if span > u128::from(u64::MAX) {
            return self.next_u64() as i64;
        }
        (i128::from(lo) + i128::from(self.below(span as u64))) as i64
    }
}

/// The shared state of a [`ScriptFake`] and its copies.
#[derive(Debug)]
struct FakeState {
    /// The seed `Fake(seed)` was created with (a fork keeps its root's seed).
    seed: u64,
    /// The stream key: the seed for the root, mixed with each fork name for a fork.
    stream: u64,
    rng: Rng,
}

/// A seeded test-data generator, registered with Rhai as `Fake` (see the module docs for the
/// reproducibility contract and the sharing of copies).
#[derive(Debug, Clone)]
pub(crate) struct ScriptFake(Arc<Mutex<FakeState>>);

/// The error of a `Fake` method called with an argument it cannot use.
fn argument_error(method: &str, expected: &str, actual: String) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(
        Dynamic::from(crate::error::format_diagnostic(
            &ScriptError::ArgumentMismatch {
                step_name: format!("Fake.{method}"),
                expected: expected.to_string(),
                actual,
            },
        )),
        rhai::Position::NONE,
    ))
}

/// Capitalizes the first character of an ASCII word.
fn capitalize(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    let mut chars = word.chars();
    if let Some(first) = chars.next() {
        out.push(first.to_ascii_uppercase());
    }
    out.extend(chars);
    out
}

/// Lowercases a name for use in a user name or an address: letters and digits only.
fn slug(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

impl ScriptFake {
    fn from_stream(seed: u64, stream: u64) -> Self {
        Self(Arc::new(Mutex::new(FakeState {
            seed,
            stream,
            rng: Rng { state: stream },
        })))
    }

    /// `Fake(seed)`: a generator whose values depend only on `seed` and the calls made.
    pub(crate) fn new(seed: INT) -> Self {
        let seed = seed as u64;
        Self::from_stream(seed, seed)
    }

    /// `Fake()`: a generator with a random seed; `seed()` tells which, to reproduce the run.
    /// Only non-negative seeds are drawn, so the reported seed reads naturally.
    pub(crate) fn random() -> Self {
        Self::new((rand::random::<u64>() >> 1) as INT)
    }

    /// Runs `f` on the shared state. A poisoned lock only means an earlier panic while it was
    /// held; the state is plain numbers and still usable.
    fn with<R>(&self, f: impl FnOnce(&mut FakeState) -> R) -> R {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut state)
    }

    fn rng<R>(&self, f: impl FnOnce(&mut Rng) -> R) -> R {
        self.with(|state| f(&mut state.rng))
    }

    /// The seed this generator (or its root, for a fork) was created with.
    pub(crate) fn seed(&self) -> INT {
        self.with(|state| state.seed as INT)
    }

    /// An independent generator for the stream `name` of this one: the same seed and fork names
    /// give the same stream, whatever was drawn from this generator before.
    pub(crate) fn fork(&self, name: &str) -> Self {
        let (seed, stream) = self.with(|state| (state.seed, state.stream));
        Self::from_stream(seed, mix64(stream ^ mix64(fnv1a(name))))
    }

    /// The text `print()` and the REPL show.
    pub(crate) fn describe(&self) -> String {
        format!("Fake(seed: {})", self.seed())
    }

    pub(crate) fn int(&self, lo: INT, hi: INT) -> Result<INT, Box<EvalAltResult>> {
        if lo > hi {
            return Err(argument_error(
                "int",
                "lo <= hi",
                format!("int({lo}, {hi})"),
            ));
        }
        Ok(self.rng(|rng| rng.int(lo, hi)))
    }

    pub(crate) fn float(&self) -> f64 {
        self.rng(Rng::unit)
    }

    pub(crate) fn float_between(&self, lo: f64, hi: f64) -> Result<f64, Box<EvalAltResult>> {
        if !(lo.is_finite() && hi.is_finite() && lo <= hi) {
            return Err(argument_error(
                "float",
                "finite bounds with lo <= hi",
                format!("float({lo}, {hi})"),
            ));
        }
        let unit = self.rng(Rng::unit);
        // `lo + unit * (hi - lo)` can round up to `hi`; keep the interval half-open.
        let value = lo + unit * (hi - lo);
        Ok(if value >= hi && lo < hi { lo } else { value })
    }

    pub(crate) fn chance(&self, p: f64) -> Result<bool, Box<EvalAltResult>> {
        if !(0.0..=1.0).contains(&p) {
            return Err(argument_error(
                "chance",
                "a probability from 0.0 to 1.0",
                p.to_string(),
            ));
        }
        Ok(self.rng(Rng::unit) < p)
    }

    pub(crate) fn pick(&self, items: Array) -> Result<Dynamic, Box<EvalAltResult>> {
        if items.is_empty() {
            return Err(argument_error(
                "pick",
                "a non-empty array",
                "an empty array".to_string(),
            ));
        }
        let index = self.rng(|rng| rng.index(items.len()));
        Ok(items.into_iter().nth(index).unwrap_or_default())
    }

    /// A shuffled copy of `items` (Fisher-Yates).
    pub(crate) fn shuffle(&self, mut items: Array) -> Array {
        self.rng(|rng| {
            for i in (1..items.len()).rev() {
                items.swap(i, rng.index(i + 1));
            }
        });
        items
    }

    /// `n` distinct positions of `items`, in random order (a partial Fisher-Yates).
    pub(crate) fn sample(&self, mut items: Array, n: INT) -> Result<Array, Box<EvalAltResult>> {
        let Some(n) = usize::try_from(n).ok().filter(|n| *n <= items.len()) else {
            return Err(argument_error(
                "sample",
                &format!("a count from 0 to the array length {}", items.len()),
                n.to_string(),
            ));
        };
        self.rng(|rng| {
            for i in 0..n {
                let j = i + rng.index(items.len() - i);
                items.swap(i, j);
            }
        });
        items.truncate(n);
        Ok(items)
    }

    pub(crate) fn uuid(&self) -> ScriptUuid {
        let (high, low) = self.rng(|rng| (rng.next_u64(), rng.next_u64()));
        script_uuid_v4((u128::from(high) << 64) | u128::from(low))
    }

    pub(crate) fn first_name(&self) -> String {
        self.rng(|rng| rng.pick(FIRST_NAMES)).to_string()
    }

    pub(crate) fn last_name(&self) -> String {
        self.rng(|rng| rng.pick(LAST_NAMES)).to_string()
    }

    pub(crate) fn full_name(&self) -> String {
        self.rng(|rng| format!("{} {}", rng.pick(FIRST_NAMES), rng.pick(LAST_NAMES)))
    }

    pub(crate) fn username(&self) -> String {
        self.rng(|rng| {
            let first = slug(rng.pick(FIRST_NAMES));
            let last = slug(rng.pick(LAST_NAMES));
            let initial = last.chars().next().unwrap_or_default();
            format!("{first}{initial}{}", rng.int(1, 999))
        })
    }

    pub(crate) fn email(&self) -> String {
        self.rng(|rng| {
            let first = slug(rng.pick(FIRST_NAMES));
            let last = slug(rng.pick(LAST_NAMES));
            let number = rng.int(1, 99);
            format!("{first}.{last}{number}@{}", rng.pick(EMAIL_DOMAINS))
        })
    }

    /// An e-mail address for `name` ("Ada Lovelace" -> `ada.lovelace17@example.org`), so a
    /// person's address matches their name.
    pub(crate) fn email_for(&self, name: &str) -> String {
        let local = name
            .split_whitespace()
            .map(slug)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(".");
        let local = if local.is_empty() {
            "user".to_string()
        } else {
            local
        };
        self.rng(|rng| {
            let number = rng.int(1, 99);
            format!("{local}{number}@{}", rng.pick(EMAIL_DOMAINS))
        })
    }

    /// A US phone number in the fictional `555-01xx` range.
    pub(crate) fn phone(&self) -> String {
        self.rng(|rng| format!("+1-{}-555-01{:02}", rng.int(201, 989), rng.int(0, 99)))
    }

    pub(crate) fn job_title(&self) -> String {
        self.rng(|rng| {
            let level = rng.pick(JOB_LEVELS);
            let area = rng.pick(JOB_AREAS);
            let role = rng.pick(JOB_ROLES);
            if level.is_empty() {
                format!("{area} {role}")
            } else {
                format!("{level} {area} {role}")
            }
        })
    }

    pub(crate) fn street_name(&self) -> String {
        self.rng(|rng| format!("{} {}", rng.pick(STREET_ROOTS), rng.pick(STREET_SUFFIXES)))
    }

    pub(crate) fn street_address(&self) -> String {
        self.rng(|rng| {
            let number = rng.int(1, 9999);
            format!(
                "{number} {} {}",
                rng.pick(STREET_ROOTS),
                rng.pick(STREET_SUFFIXES)
            )
        })
    }

    pub(crate) fn city(&self) -> String {
        self.rng(|rng| rng.pick(CITIES).0).to_string()
    }

    pub(crate) fn state(&self) -> String {
        self.rng(|rng| rng.pick(STATES).0).to_string()
    }

    pub(crate) fn state_abbr(&self) -> String {
        self.rng(|rng| rng.pick(STATES).1).to_string()
    }

    pub(crate) fn zip_code(&self) -> String {
        self.rng(|rng| format!("{:05}", rng.int(1001, 99950)))
    }

    pub(crate) fn country(&self) -> String {
        self.rng(|rng| rng.pick(COUNTRIES)).to_string()
    }

    /// A one-line US address whose city and state belong together.
    pub(crate) fn address(&self) -> String {
        self.rng(|rng| {
            let number = rng.int(1, 9999);
            let root = rng.pick(STREET_ROOTS);
            let suffix = rng.pick(STREET_SUFFIXES);
            let (city, state) = rng.pick(CITIES);
            let zip = rng.int(1001, 99950);
            format!("{number} {root} {suffix}, {city}, {state} {zip:05}")
        })
    }

    pub(crate) fn company(&self) -> String {
        self.rng(|rng| match rng.below(3) {
            0 => format!("{} {}", rng.pick(LAST_NAMES), rng.pick(COMPANY_SUFFIXES)),
            1 => format!("{} & {}", rng.pick(LAST_NAMES), rng.pick(LAST_NAMES)),
            _ => format!(
                "{} {} {}",
                capitalize(rng.pick(WORDS)),
                capitalize(rng.pick(WORDS)),
                rng.pick(COMPANY_SUFFIXES)
            ),
        })
    }

    pub(crate) fn domain_name(&self) -> String {
        self.rng(|rng| {
            let name = slug(rng.pick(LAST_NAMES));
            format!("{name}{}.{}", rng.pick(WORDS), rng.pick(TLDS))
        })
    }

    pub(crate) fn word(&self) -> String {
        self.rng(|rng| rng.pick(WORDS)).to_string()
    }

    /// A sentence of 4 to 12 words, capitalized, ending with a period.
    pub(crate) fn sentence(&self) -> String {
        self.rng(|rng| {
            let count = rng.int(4, 12);
            let mut out = String::new();
            for i in 0..count {
                let word = rng.pick(WORDS);
                if i == 0 {
                    out.push_str(&capitalize(word));
                } else {
                    out.push(' ');
                    out.push_str(word);
                }
            }
            out.push('.');
            out
        })
    }

    /// A Lorem ipsum sentence of 6 to 14 words, sometimes with a comma.
    pub(crate) fn lorem_sentence(&self) -> String {
        self.rng(lorem_sentence)
    }

    /// A Lorem ipsum paragraph of 3 to 7 sentences.
    pub(crate) fn lorem_paragraph(&self) -> String {
        self.rng(lorem_paragraph)
    }

    /// Lorem ipsum text of a random length from `lo` to `hi` bytes (both included): paragraphs
    /// separated by an empty line, starting with "Lorem ipsum dolor sit amet", cut to the
    /// drawn length and ending with a period. The text is ASCII, so bytes are characters.
    pub(crate) fn lorem(&self, lo: INT, hi: INT) -> Result<String, Box<EvalAltResult>> {
        if lo < 0 || lo > hi || hi > MAX_LOREM_BYTES {
            return Err(argument_error(
                "lorem",
                &format!("0 <= lo <= hi <= {MAX_LOREM_BYTES}"),
                format!("lo = {lo}, hi = {hi}"),
            ));
        }
        Ok(self.rng(|rng| {
            let len = rng.int(lo, hi) as usize;
            let mut out = String::with_capacity(len + 512);
            out.push_str(LOREM_START);
            while out.len() < len {
                out.push_str("\n\n");
                out.push_str(&lorem_paragraph(rng));
            }
            out.truncate(len);
            // The cut can end inside a word or on a separator: a period closes the text.
            if out.pop().is_some() {
                out.push('.');
            }
            out
        }))
    }
}

/// The upper bound of [`ScriptFake::lorem`]: the default script string limit (16 MiB).
const MAX_LOREM_BYTES: INT = 16 * 1024 * 1024;

/// How every [`ScriptFake::lorem`] text starts.
const LOREM_START: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit.";

fn lorem_sentence(rng: &mut Rng) -> String {
    let count = rng.int(6, 14);
    let comma_after = if rng.below(3) == 0 {
        rng.int(2, count - 2)
    } else {
        -1
    };
    let mut out = String::new();
    for i in 0..count {
        let word = rng.pick(LOREM_WORDS);
        if i == 0 {
            out.push_str(&capitalize(word));
        } else {
            out.push(' ');
            out.push_str(word);
        }
        if i == comma_after {
            out.push(',');
        }
    }
    out.push('.');
    out
}

fn lorem_paragraph(rng: &mut Rng) -> String {
    let count = rng.int(3, 7);
    let mut out = String::new();
    for i in 0..count {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&lorem_sentence(rng));
    }
    out
}

/// Registers the `Fake` type, its constructors `Fake()` / `Fake(seed)` and its methods in both
/// spellings.
pub(super) fn register(registry: &mut Registry) {
    registry
        .register_type_with_name::<ScriptFake>("Fake")
        .register_fn("Fake", ScriptFake::random)
        .register_fn("Fake", ScriptFake::new)
        .register_fn("to_string", |f: &mut ScriptFake| f.describe())
        .register_fn("to_debug", |f: &mut ScriptFake| f.describe())
        .register_fn("seed", |f: &mut ScriptFake| f.seed())
        .register_fn("fork", |f: &mut ScriptFake, name: &str| f.fork(name))
        .register_fn("int", |f: &mut ScriptFake, lo: INT, hi: INT| f.int(lo, hi))
        .register_fn("float", |f: &mut ScriptFake| f.float())
        .register_fn("float", |f: &mut ScriptFake, lo: f64, hi: f64| {
            f.float_between(lo, hi)
        })
        .register_fn("float", |f: &mut ScriptFake, lo: INT, hi: INT| {
            f.float_between(lo as f64, hi as f64)
        })
        .register_fn("chance", |f: &mut ScriptFake, p: f64| f.chance(p))
        .register_fn("pick", |f: &mut ScriptFake, items: Array| f.pick(items))
        .register_fn("shuffle", |f: &mut ScriptFake, items: Array| {
            f.shuffle(items)
        })
        .register_fn("sample", |f: &mut ScriptFake, items: Array, n: INT| {
            f.sample(items, n)
        })
        .register_fn("uuid", |f: &mut ScriptFake| f.uuid())
        .register_fn_as(&["first_name"], |f: &mut ScriptFake| f.first_name())
        .register_fn_as(&["last_name"], |f: &mut ScriptFake| f.last_name())
        .register_fn_as(&["full_name"], |f: &mut ScriptFake| f.full_name())
        .register_fn("username", |f: &mut ScriptFake| f.username())
        .register_fn("email", |f: &mut ScriptFake| f.email())
        .register_fn("email", |f: &mut ScriptFake, name: &str| f.email_for(name))
        .register_fn("phone", |f: &mut ScriptFake| f.phone())
        .register_fn_as(&["job_title"], |f: &mut ScriptFake| f.job_title())
        .register_fn_as(&["street_name"], |f: &mut ScriptFake| f.street_name())
        .register_fn_as(&["street_address"], |f: &mut ScriptFake| f.street_address())
        .register_fn("city", |f: &mut ScriptFake| f.city())
        .register_fn("state", |f: &mut ScriptFake| f.state())
        .register_fn_as(&["state_abbr"], |f: &mut ScriptFake| f.state_abbr())
        .register_fn_as(&["zip_code"], |f: &mut ScriptFake| f.zip_code())
        .register_fn("country", |f: &mut ScriptFake| f.country())
        .register_fn("address", |f: &mut ScriptFake| f.address())
        .register_fn("company", |f: &mut ScriptFake| f.company())
        .register_fn_as(&["domain_name"], |f: &mut ScriptFake| f.domain_name())
        .register_fn("word", |f: &mut ScriptFake| f.word())
        .register_fn("sentence", |f: &mut ScriptFake| f.sentence())
        .register_fn("lorem", |f: &mut ScriptFake, lo: INT, hi: INT| {
            f.lorem(lo, hi)
        })
        .register_fn_as(&["lorem_sentence"], |f: &mut ScriptFake| f.lorem_sentence())
        .register_fn_as(&["lorem_paragraph"], |f: &mut ScriptFake| {
            f.lorem_paragraph()
        });
}
