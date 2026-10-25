# Generating Test Data

`Fake(seed)` is a generator of test data for scripts: English names, e-mail addresses, job
titles, addresses, companies and words, plus the random numbers a script needs to build a graph's
shape (how many reports a manager has, who knows whom). It is part of the Rhai DSL (feature
`script`), so it works in the CLI, the playground, the dev server and Python alike.

```rhai
let f = Fake(42);
f.full_name()             // "Lawrence Young"
f.int(1, 6)               // 2: a die roll
f.email("Ada Lovelace")   // "ada.lovelace35@example.com"
```

## Reproducible by design

The same seed and the same sequence of calls give the same values, on every platform (native and
WebAssembly) and in every run. A graph generated from a seed can be rebuilt exactly, to reproduce
a bug, to compare two versions of a query, or to benchmark on the same data.

- `Fake()` without a seed picks a random one; `f.seed()` tells which, so a run you liked can be
  repeated with `Fake(that_seed)`.
- The generator is Graphersal's own (SplitMix64), not the `rand` crate's, whose algorithms may
  change between versions. The word lists and the way each method draws are part of the contract
  as well: a change to them changes what every seed produces, so it is treated as a breaking
  change (a test pins the values of seed 42).
- `random.seed` (`g.with("random.seed", n)`) seeds the random steps of one traversal (`coin()`,
  `sample()`, `order().by(Order.shuffle)`); `Fake` is independent of it.

## Copies share one stream

A `Fake` value is a handle. A copy (`let b = a;`, a function argument) draws from the same
stream, so a helper function advances the caller's generator and every call gets new values:

```rhai
fn person(f) { #{ name: f.full_name(), age: f.int(18, 90) } }
let f = Fake(1);
[person(f), person(f)]   // two different people
```

## Forks: independent streams

`f.fork(name)` returns a generator for the stream `name` of the same seed. A fork depends only on
the seed and its name (and the names of the forks above it), never on what was drawn before. Give
each part of a generator its own fork, and drawing one more value in one part does not shift the
values of all the others:

```rhai
let f = Fake(42);
let people = f.fork("people");
let social = f.fork("social");   // unchanged when the people part draws more values
```

## Methods

| Method | Result |
|---|---|
| `seed()` | The seed (of the root, for a fork) |
| `fork(name)` | An independent generator for the named stream |
| `int(lo, hi)` | An integer from `lo` to `hi`, both included |
| `float()`, `float(lo, hi)` | A float in `[0, 1)` or `[lo, hi)` |
| `chance(p)` | `true` with probability `p` (0.0 to 1.0) |
| `pick(array)` | A random element of a non-empty array |
| `shuffle(array)` | A copy of the array in random order |
| `sample(array, n)` | `n` distinct elements of the array, in random order |
| `uuid()` | A version 4 UUID value (the `Uuid` type, see [UUID Values](uuid_values.md)) |
| `first_name()`, `last_name()`, `full_name()` | Common US given names and surnames |
| `username()` | `victoriav971` |
| `email()`, `email(name)` | An address at `example.com`/`.org`/`.net` (RFC 2606); with a name, made from it |
| `phone()` | A US number in the fictional `555-01xx` range |
| `job_title()` | `Senior Data Analyst` |
| `street_name()`, `street_address()` | `Lakeview Circle`, `5666 Broad Trail` |
| `city()`, `state()`, `state_abbr()`, `zip_code()`, `country()` | Real US cities and states, countries |
| `address()` | One line; its city and state belong together |
| `company()`, `domain_name()` | `Vargas & Ryan`, `vasquezsalt.info` |
| `word()`, `sentence()` | English words; a sentence of 4 to 12 of them |
| `lorem(lo, hi)` | Lorem ipsum text of a random length from `lo` to `hi` bytes (both included, at most 16 MiB): paragraphs separated by an empty line, starting with "Lorem ipsum dolor sit amet", ending with a period; for large text fields, e.g. to try [compressed properties](compressed_properties.md) |
| `lorem_sentence()`, `lorem_paragraph()` | a Lorem ipsum sentence of 6 to 14 words; a paragraph of 3 to 7 sentences |

Methods of two words are snake_case only (`full_name`, `zip_code`, ...), like every Graphersal
extension.
A wrong argument (`int(5, 1)`, `chance(1.5)`, `pick([])`, `sample([1], 2)`) is a script error that
names the method.

Only English data exists for now. `Fake` has no date method; build a random date from a number:
`date("2024-01-01") + duration("P" + f.int(0, 365) + "D")` ([Dates and Times](../dates/index.md)).

## Building a graph

The playground's Examples menu has a complete generator under *Generated test data (Fake)*: its
parameters (seed, number of companies, depth and fan-out of the management trees, number of
`knows` edges, number of cities) are Rhai variables at the top of the script. Its core:

```rhai
let f = Fake(42);
let people = f.fork("people");
let ids = [];
for i in 0..100 {
    let name = people.full_name();
    ids.push(g.add_v("person").property("name", name)
        .property("email", people.email(name)).property("age", people.int(21, 67))
        .id().next());
}
let social = f.fork("social");
for k in 0..300 {
    let a = ids[social.int(0, ids.len() - 1)];
    let b = ids[social.int(0, ids.len() - 1)];
    if a != b { g.v(a).add_e("knows").to(g.v(b)).to_list(); }
}
```

Pick from a large array by index, as above: `pick()`, `shuffle()` and `sample()` receive a copy of
the array they are given, which costs one copy per call. The example's largest setting (3
companies, depth 6, fan-out 4: 16 383 people, 50 000 `knows` edges) takes about a second in a
native release build.
