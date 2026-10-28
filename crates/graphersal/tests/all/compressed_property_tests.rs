//! Compressed properties (`book/src/users_guide/compressed_properties.md`, book page "Compressed
//! Properties"): rules as catalog definitions of kind `compression`, the storage-only compressed
//! representation, the invariant after every operation, reads that are always plain, change
//! capture, transactions, persistence, exchange formats, schema, permissions, a randomized
//! differential test and the DSL.
#![cfg(feature = "script")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use graphersal::catalog::{Codec, DEFAULT_MIN_BYTES, MAX_MIN_BYTES};
use graphersal::catalog::{Definition, DefinitionKind};
use graphersal::changes::ChangeSet;
use graphersal::prelude::*;
use graphersal::prelude::{Graph, PropertyMap};
use graphersal::{
    auth::AccessPolicy,
    catalog::CompressionDefinition,
    changes::{CommitHook, HookError, Mutation},
    storage::ElementKind,
};

use crate::script_host::*;

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

/// A deterministic CV-like text of about `len` bytes (some multi-byte characters included).
fn cv(seed: usize, len: usize) -> String {
    const PHRASES: [&str; 12] = [
        "Senior software engineer with a focus on distributed systems. ",
        "Led the platform team of twelve engineers at Acme Corp. ",
        "Designed and built a graph database engine in Rust. ",
        "Mentored junior developers and ran the hiring loop. ",
        "Introduced continuous delivery and cut release times by 80 %. ",
        "Speaker at RustConf and GraphConnect. ",
        "Education: M.Sc. Computer Science, Technische Universität München. ",
        "Languages: English, Deutsch, Français, 日本語. ",
        "Skills: Rust, Go, Kubernetes, PostgreSQL, Kafka. ",
        "Responsible for the on-call rotation and incident reviews. ",
        "Migrated a monolith to services without downtime. ",
        "Hobbies: climbing, chess, open source. ",
    ];
    let mut out = format!("CV #{seed}: ");
    let mut i = seed;
    while out.len() < len {
        out.push_str(PHRASES[i % PHRASES.len()]);
        i = i.wrapping_mul(7).wrapping_add(3);
    }
    // Cut at a character boundary.
    let mut end = len.min(out.len());
    while !out.is_char_boundary(end) {
        end -= 1;
    }
    out.truncate(end);
    out
}

fn object(entries: Vec<(&str, ElementProperty)>) -> ElementProperty {
    let mut map = PropertyMap::default();
    for (key, value) in entries {
        map.insert(key, value);
    }
    ElementProperty::Object(Box::new(map))
}

fn props(entries: Vec<(&str, ElementProperty)>) -> PropertyMap {
    let mut map = PropertyMap::default();
    for (key, value) in entries {
        map.insert(key, value);
    }
    map
}

/// Customers (some also `vip`), products and `bought` edges with notes.
fn populate(graph: &mut TraversalGraph) {
    for i in 0..12 {
        let mut labels = vec!["customer".into()];
        if i % 3 == 0 {
            labels.push("vip".into());
        }
        let bio = if i % 4 == 1 {
            "short bio".to_string()
        } else {
            cv(i, 300 + 37 * i)
        };
        let mut entries = vec![
            ("name", ElementProperty::from(format!("c{i}"))),
            ("bio", ElementProperty::from(bio)),
            (
                "details",
                object(vec![
                    (
                        "career",
                        object(vec![
                            ("cv", ElementProperty::from(cv(100 + i, 800 + 91 * i))),
                            ("years", ElementProperty::from(i as i64)),
                        ]),
                    ),
                    ("city", ElementProperty::from("Berlin")),
                ]),
            ),
        ];
        if i % 3 == 0 {
            entries.push(("summary", ElementProperty::from(cv(200 + i, 400))));
        }
        graph
            .add_vertex(Some(format!("c{i}").into()), labels, props(entries))
            .unwrap();
    }
    for i in 0..4 {
        graph
            .add_vertex(
                Some(format!("p{i}").into()),
                vec!["product".into()],
                props(vec![
                    ("name", ElementProperty::from(format!("p{i}"))),
                    ("bio", ElementProperty::from(cv(300 + i, 500))),
                ]),
            )
            .unwrap();
    }
    for i in 0..12 {
        let notes = if i % 2 == 0 {
            cv(400 + i, 350)
        } else {
            "fast delivery".to_string()
        };
        graph
            .add_edge_by_ids(
                Some(format!("b{i}").into()),
                Some("bought".into()),
                &format!("c{i}"),
                &format!("p{}", i % 4),
                props(vec![("notes", ElementProperty::from(notes))]),
            )
            .unwrap();
    }
}

fn rule(element: ElementKind, label: &str, path: &str) -> CompressionDefinition {
    CompressionDefinition::new(element, label, path).unwrap()
}

/// The rules of the differential battery: two paths on `customer`, an edge rule, a rule on the
/// second label of a multi-label vertex.
fn define_rules(graph: &mut TraversalGraph) {
    let mut g = graph.traversal_mut();
    g.define_compression("bio", rule(ElementKind::Vertex, "customer", "bio"))
        .unwrap();
    g.define_compression(
        "career_cv",
        rule(ElementKind::Vertex, "customer", "details.career.cv").with_min_bytes(512),
    )
    .unwrap();
    g.define_compression("notes", rule(ElementKind::Edge, "bought", "notes"))
        .unwrap();
    g.define_compression(
        "vip_summary",
        rule(ElementKind::Vertex, "vip", "summary").with_dictionary(false),
    )
    .unwrap();
}

fn plain_graph() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    populate(&mut graph);
    graph
}

fn compressed_graph() -> TraversalGraph {
    let mut graph = plain_graph();
    define_rules(&mut graph);
    graph
}

fn shared(graph: TraversalGraph) -> Arc<Graph> {
    Arc::new(Graph::new(graph))
}

fn invariant(graph: &TraversalGraph) {
    if let Err(reason) = graph.check_compression_invariant() {
        panic!("invariant broken: {reason}");
    }
}

/// How many stored values are compressed (memory counters).
fn compressed_count(graph: &TraversalGraph) -> u64 {
    graph.memory_usage().unwrap().unwrap().compressed_values
}

/// The logical content of a graph: every vertex and edge with its labels and properties, by id
/// (compressed values compare as their strings).
fn content(graph: &TraversalGraph) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in graph.vertices().unwrap() {
        let mut props: Vec<(String, String)> = graph
            .vertex_properties(v)
            .unwrap()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.to_string(), format!("{:?}", v.to_logical())))
            .collect();
        props.sort();
        out.push(format!(
            "v {} {:?} {props:?}",
            graph.vertex_id(v).unwrap().unwrap(),
            graph.vertex_labels(v).unwrap()
        ));
    }
    for e in graph.edges().unwrap() {
        let mut props: Vec<(String, String)> = graph
            .edge_properties(e)
            .unwrap()
            .map(|p| {
                p.iter()
                    .map(|(k, v)| (k.to_string(), format!("{:?}", v.to_logical())))
                    .collect()
            })
            .unwrap_or_default();
        props.sort();
        out.push(format!(
            "e {} {:?} {props:?}",
            graph.edge_id(e).unwrap().unwrap(),
            graph.edge_label(e).unwrap()
        ));
    }
    out.sort();
    out
}

/// Expands `"abc".repeat(N)` in a script into the literal (Rhai strings have no `repeat`).
fn expand(script: &str) -> String {
    let mut out = String::new();
    let mut rest = script;
    while let Some(at) = rest.find(".repeat(") {
        let (head, tail) = rest.split_at(at);
        let open = head.rfind('"').unwrap();
        let quote_start = head[..open].rfind('"').unwrap();
        let text = &head[quote_start + 1..open];
        let close = tail.find(')').unwrap();
        let count: usize = tail[".repeat(".len()..close].parse().unwrap();
        out.push_str(&head[..quote_start]);
        out.push('"');
        out.push_str(&text.repeat(count));
        out.push('"');
        rest = &tail[close + 1..];
    }
    out.push_str(rest);
    out
}

fn no_compressed_inside(value: &ElementProperty) -> bool {
    !value.holds_compressed()
}

// ---------------------------------------------------------------------------------------------
// 1. Representation
// ---------------------------------------------------------------------------------------------

#[test]
fn the_value_stays_one_word_and_compares_logically() {
    assert_eq!(std::mem::size_of::<ElementProperty>(), 32);
    let graph = compressed_graph();
    let c0 = graph.get_vertex_by_id("c0").unwrap().unwrap();
    let stored = graph
        .vertex_properties(c0)
        .unwrap()
        .unwrap()
        .get("bio")
        .unwrap();
    assert!(
        matches!(stored, ElementProperty::Compressed(_)),
        "{stored:?}"
    );
    let plain = ElementProperty::from(cv(0, 300));
    assert_eq!(stored, &plain);
    assert_eq!(&plain, stored);
    let hash = |value: &ElementProperty| {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    };
    assert_eq!(hash(stored), hash(&plain));
    assert!(stored.is_text());
    assert_eq!(stored.text().unwrap(), cv(0, 300));
    assert_eq!(stored.to_logical(), plain);
    assert!(!stored.to_logical().holds_compressed());
}

#[test]
fn thresholds_multibyte_text_and_incompressible_text() {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .define_compression(
            "t",
            rule(ElementKind::Vertex, "doc", "text").with_min_bytes(100),
        )
        .unwrap();
    let repetitive = "abcabcabc".repeat(20);
    let texts = [
        "".to_string(),
        repetitive[..99].to_string(),
        repetitive[..100].to_string(),
        "Grüße aus München — 東京 🎉 ".repeat(20),
        // Pseudo-random printable bytes: LZ4 does not reach 90 %, so it stays plain.
        {
            let mut state = 0x9E37_79B9_7F4A_7C15u64;
            (0..400)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    char::from(b'!' + (state % 90) as u8)
                })
                .collect()
        },
    ];
    let expect_compressed = [false, false, true, true, false];
    for (i, text) in texts.iter().enumerate() {
        let v = graph
            .add_vertex(
                None,
                vec!["doc".into()],
                props(vec![("text", text.as_str().into())]),
            )
            .unwrap();
        let stored = graph
            .vertex_properties(v)
            .unwrap()
            .unwrap()
            .get("text")
            .unwrap();
        assert_eq!(
            matches!(stored, ElementProperty::Compressed(_)),
            expect_compressed[i],
            "text {i} ({} bytes)",
            text.len()
        );
        let read = graph
            .traversal()
            .v(vec![graph.vertex_id(v).unwrap().unwrap()])
            .values("text")
            .to_list()
            .unwrap();
        assert_eq!(read, vec![ElementProperty::from(text.as_str())]);
    }
    invariant(&graph);
}

// ---------------------------------------------------------------------------------------------
// 2. Differential read battery
// ---------------------------------------------------------------------------------------------

/// Queries that read compressed values in every way; each must give the same result on the
/// graph without rules.
const BATTERY: &[&str] = &[
    r#"g.v().has_label("customer").values("bio").to_list()"#,
    r#"g.v().values("details").to_list()"#,
    r#"g.v().value_map().to_list()"#,
    r#"g.v().value_map(true).to_list()"#,
    r#"g.v().element_map().to_list()"#,
    r#"g.e().element_map().to_list()"#,
    r#"g.v().properties("bio").value().to_list()"#,
    r#"g.v().properties("bio").to_list()"#,
    r#"g.v().values(jpath("details.career.cv")).to_list()"#,
    r#"g.v().properties(jpath("details.career.cv")).value().to_list()"#,
    r#"g.v().value_map(jpath("details.career.cv")).to_list()"#,
    r#"let x = g.v("c2").values("bio").next(); g.v().has("bio", x).values("name").to_list()"#,
    r#"let x = g.v("c2").values("bio").next(); g.v().has_label("customer").has("bio", x).count().next()"#,
    r#"let x = g.v("c5").values(jpath("details.career.cv")).next(); g.v().has(jpath("details.career.cv"), x).values("name").to_list()"#,
    r#"g.v().has("bio", TextP.containing("Rust")).values("name").to_list()"#,
    r#"g.v().has("bio", TextP.starting_with("CV #3")).values("name").to_list()"#,
    r#"g.v().has("bio", TextP.ending_with("open source. ")).values("name").to_list()"#,
    r#"g.v().has("bio", TextP.regex(".*Acme.*")).count().next()"#,
    r#"g.v().has("bio", TextP.not_containing("Rust")).values("name").to_list()"#,
    r#"g.v().has("bio", P.gt("CV #5")).values("name").to_list()"#,
    r#"g.v().has("bio", P.within(["short bio", "nothing"])).values("name").to_list()"#,
    r#"g.v().has(jpath("details.career.cv"), TextP.containing("Kafka")).count().next()"#,
    r#"g.v().values("bio").is(TextP.containing("chess")).count().next()"#,
    r#"g.v().has_label("customer").order().by("bio").values("name").to_list()"#,
    r#"g.v().has_label("customer").order().by("bio", Order.desc).values("name").to_list()"#,
    r#"g.v().has_label("customer").values("bio").order().to_list()"#,
    r#"g.v().has_label("customer").dedup().by("bio").count().next()"#,
    r#"g.v().values("bio").dedup().count().next()"#,
    r#"g.v().has_label("customer").group().by("bio").by("name").next()"#,
    r#"g.v().has_label("customer").group_count().by("bio").next()"#,
    r#"g.v().has_label("customer").project("n", "b").by("name").by("bio").to_list()"#,
    r#"g.v().has_label("customer").out_e("bought").in_v().path().by("bio").to_list()"#,
    r#"g.v().has_label("customer").as("a").select("a").by("bio").to_list()"#,
    r#"g.v().has_label("customer").values("bio").as("b").select("b").to_list()"#,
    r#"g.e().values("notes").to_list()"#,
    r#"g.e().has("notes", TextP.containing("fast")).count().next()"#,
    r#"g.e().has_label("bought").order().by("notes").id().to_list()"#,
    r#"g.v().has_label("vip").values("summary").to_list()"#,
    r#"g.v().has_label("customer").values("bio").length().to_list()"#,
    r#"g.v().has_label("customer").values("bio").to_upper().to_list()"#,
    r#"g.v().has_label("customer").values("bio").substring(0, 10).to_list()"#,
    r#"g.v().has_label("customer").values("bio").as_string().to_list()"#,
    r#"g.v().has_label("customer").values("bio").fold().to_list()"#,
    r#"g.v().has_label("customer").values("bio").max().next()"#,
    r#"g.v().has_label("customer").values("bio").min().next()"#,
    r#"g.v().has_label("customer").map(__.values("bio")).to_list()"#,
    r#"g.v().has_label("customer").values("details").unfold().to_list()"#,
    r#"g.v().has_label("customer").values("bio").concat("!").to_list()"#,
    r#"g.v("c0").values("bio").split(" ").to_list()"#,
    r#"g.v().has_label("customer").limit(3).value_map("name", "bio").to_json()"#,
    r#"g.v().has_label("customer").limit(3).value_map("name", "bio").to_table()"#,
    r#"g.v().has_label("customer").limit(3).values("bio").to_table()"#,
    r#"g.e().has_label("bought").subgraph("sg").cap("sg").next()"#,
    r#"g.e().has_label("bought").limit(3).to_graph()"#,
    r#"g.infer_schema()"#,
    r#"g.v().has("bio", P.typeOf(GType.STRING)).count().next()"#,
    r#"g.v("c0").properties("bio").as_string().to_list()"#,
    r#"g.v().has_label("customer").limit(3).values("bio").fold().conjoin("|").to_list()"#,
    r#"g.v().has_label("customer").values("bio").as_("x").where(__.select("x").is(TextP.containing("Rust"))).count().next()"#,
    r#"g.v().has_label("customer").values("details").select(Column.values).to_list()"#,
    r#"g.v().has_label("customer").values("details").unfold().select(Column.values).to_list()"#,
    r#"g.v().has_label("customer").limit(2).values("bio").sample(1).count().next()"#,
    r#"g.v().has_label("customer").values("bio").group_count().next()"#,
    r#"g.v().has_label("customer").has("bio").has_not("summary").values("name").to_list()"#,
    r#"g.v().has_label("customer").values("bio").is(P.typeOf(GType.STRING)).count().next()"#,
    r#"g.v().has_label("customer").limit(2).values("details").json_path("$.career.cv").to_list()"#,
    r#"g.v().has_label("customer").limit(2).to_json()"#,
    r#"g.v().has_label("customer").limit(2).values("bio").to_json()"#,
];

#[test]
fn every_read_gives_the_same_result_with_and_without_rules() {
    let plain = shared(plain_graph());
    let compressed = shared(compressed_graph());
    assert!(compressed_count(&compressed.read()) > 10);
    for query in BATTERY {
        let expected = eval_value(plain.clone(), query)
            .unwrap_or_else(|err| panic!("{query}: {err}"))
            .to_string();
        let actual = eval_value(compressed.clone(), query)
            .unwrap_or_else(|err| panic!("{query}: {err}"))
            .to_string();
        assert_eq!(actual, expected, "{query}");
    }
    invariant(&compressed.read());
}

#[test]
fn rust_reads_and_materialized_results_are_plain() {
    let plain = plain_graph();
    let compressed = compressed_graph();
    let read = |graph: &TraversalGraph| {
        let g = graph.traversal();
        (
            g.v(None).value_map(None::<()>).to_list().unwrap(),
            graph.traversal().v(None).to_list().unwrap(),
            graph.traversal().e(None).to_list().unwrap(),
            graph.traversal().v(None).values("bio").to_list().unwrap(),
        )
    };
    let (a, b, c, d) = read(&compressed);
    assert_eq!((a.clone(), b.clone(), c.clone(), d.clone()), read(&plain));
    for value in a.iter().chain(&b).chain(&c).chain(&d) {
        assert!(no_compressed_inside(value), "{value:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// 3. The invariant after every write
// ---------------------------------------------------------------------------------------------

#[test]
fn the_invariant_holds_after_every_kind_of_write() {
    let graph = shared(compressed_graph());
    let steps = [
        r#"g.v("c1").property("bio", "x".repeat(600)).to_list()"#,
        r#"g.v("c1").property("bio", "tiny").to_list()"#,
        r#"g.v("c2").property(jpath("details.career.cv"), "y".repeat(900)).to_list()"#,
        r#"g.v("c2").property(jpath("details.career.cv"), "short").to_list()"#,
        r#"g.v("c3").property("details", #{career: #{cv: "z".repeat(700), years: 1}}).to_list()"#,
        r#"g.v("c3").property(jpath("details.career"), #{cv: "w".repeat(700)}).to_list()"#,
        r#"g.add_v("customer").property("name", "new").property("bio", "q".repeat(500)).to_list()"#,
        r#"g.add_v("customer").property("bio", "folded ".repeat(80)).property("details", #{career: #{cv: "f".repeat(600)}}).to_list()"#,
        r#"g.v("c4").remove_property("bio").to_list()"#,
        r#"g.v("c4").remove_property(jpath("details.career.cv")).to_list()"#,
        r#"g.v("c5").drop().to_list()"#,
        r#"g.merge_v(#{"T.label": "customer", name: "merged"}).option(Merge.onCreate, #{bio: "m".repeat(400)}).to_list()"#,
        r#"g.merge_v(#{"T.label": "customer", name: "merged"}).option(Merge.onMatch, #{bio: "n".repeat(400)}).to_list()"#,
        r#"g.v("c6").add_e("bought").to(__.v("p1")).property("notes", "note ".repeat(100)).to_list()"#,
        r#"g.e("b0").property("notes", "e".repeat(300)).to_list()"#,
        r#"g.e("b0").property("notes", "edge").to_list()"#,
        r#"g.v("c7").property("summary", "s".repeat(500)).to_list()"#,
        r#"g.define_compression(#{name: "bio", label: "customer", path: "bio", min_bytes: 4000})"#,
        r#"g.define_compression(#{name: "bio", label: "customer", path: "bio", min_bytes: 10, dictionary: false})"#,
        r#"g.define_compression(#{name: "city", label: "customer", path: "details.city", min_bytes: 1})"#,
        r#"g.recompress("career_cv")"#,
        r#"g.drop_compression("career_cv")"#,
        r#"g.drop_compression("bio")"#,
        r#"g.define_compression(#{name: "bio2", label: "customer", path: "bio"})"#,
        r#"g.define_compression(#{name: "notes", element: "edge", label: "bought", path: "notes", min_bytes: 1000})"#,
    ];
    for step in steps {
        let _ =
            eval_value(graph.clone(), &expand(step)).unwrap_or_else(|err| panic!("{step}: {err}"));
        invariant(&graph.read());
    }
}

#[test]
fn a_label_change_compresses_and_decompresses() {
    let mut graph = compressed_graph();
    // c1 is a customer, not a vip: its summary (none) - give it one first.
    let c1 = graph.get_vertex_by_id("c1").unwrap().unwrap();
    graph
        .set_vertex_property(c1, "summary".into(), cv(1, 600).into())
        .unwrap();
    let is_compressed = |graph: &TraversalGraph| {
        matches!(
            graph.vertex_properties(c1).unwrap().unwrap().get("summary"),
            Some(ElementProperty::Compressed(_))
        )
    };
    assert!(!is_compressed(&graph));
    graph.add_vertex_label(c1, "vip".into()).unwrap();
    assert!(is_compressed(&graph));
    invariant(&graph);
    graph.remove_vertex_label(c1, "vip").unwrap();
    assert!(!is_compressed(&graph));
    invariant(&graph);
}

#[test]
fn the_first_label_wins_a_shared_path() {
    let mut graph = TraversalGraph::new();
    {
        let mut g = graph.traversal_mut();
        g.define_compression(
            "a",
            rule(ElementKind::Vertex, "a", "text").with_min_bytes(1000),
        )
        .unwrap();
        g.define_compression(
            "b",
            rule(ElementKind::Vertex, "b", "text").with_min_bytes(10),
        )
        .unwrap();
    }
    let text = "abc".repeat(100);
    let ab = graph
        .add_vertex(
            None,
            vec!["a".into(), "b".into()],
            props(vec![("text", text.as_str().into())]),
        )
        .unwrap();
    let ba = graph
        .add_vertex(
            None,
            vec!["b".into(), "a".into()],
            props(vec![("text", text.as_str().into())]),
        )
        .unwrap();
    let compressed = |v| {
        matches!(
            graph.vertex_properties(v).unwrap().unwrap().get("text"),
            Some(ElementProperty::Compressed(_))
        )
    };
    assert!(!compressed(ab), "label a (1000 bytes) wins");
    assert!(compressed(ba), "label b (10 bytes) wins");
    invariant(&graph);
}

#[test]
fn a_rule_defined_on_an_empty_label_compresses_later_writes() {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .define_compression("bio", rule(ElementKind::Vertex, "customer", "bio"))
        .unwrap();
    graph
        .traversal_mut()
        .add_v("customer")
        .property("bio", cv(1, 2000).as_str())
        .to_list()
        .unwrap();
    assert_eq!(compressed_count(&graph), 1);
    let stats = graph.compression_stats("bio").unwrap();
    assert_eq!(
        stats.dictionary_bytes, 0,
        "no dictionary before a recompress"
    );
    graph.traversal_mut().recompress("bio").unwrap();
    let stats = graph.compression_stats("bio").unwrap();
    assert!(stats.dictionary_bytes > 0);
    assert_eq!(stats.compressed_values, 1);
    invariant(&graph);
}

// ---------------------------------------------------------------------------------------------
// 4. Memory
// ---------------------------------------------------------------------------------------------

#[test]
fn memory_counters_and_a_clear_saving() {
    let build = |rules: bool| {
        let mut graph = TraversalGraph::new();
        if rules {
            graph
                .traversal_mut()
                .define_compression("cv", rule(ElementKind::Vertex, "person", "cv"))
                .unwrap();
        }
        for i in 0..10_000 {
            graph
                .add_vertex(
                    None,
                    vec!["person".into()],
                    props(vec![("cv", cv(i, 2000 + (i % 7) * 300).into())]),
                )
                .unwrap();
        }
        if rules {
            graph.traversal_mut().recompress("cv").unwrap();
        }
        graph
    };
    let plain = build(false).memory_usage().unwrap().unwrap();
    let compressed_graph = build(true);
    let compressed = compressed_graph.memory_usage().unwrap().unwrap();
    assert_eq!(plain.compressed_values, 0);
    assert_eq!(compressed.compressed_values, 10_000);
    assert!(compressed.compressed_plain_bytes > compressed.compressed_stored_bytes * 2);
    assert!(compressed.compression_dictionary_bytes > 0);
    assert!(
        compressed.property_bytes * 2 < plain.property_bytes,
        "{} vs {}",
        compressed.property_bytes,
        plain.property_bytes
    );
    assert_eq!(
        compressed.total_bytes(),
        compressed.structure_bytes
            + compressed.index_bytes
            + compressed.property_bytes
            + compressed.compression_dictionary_bytes
    );
    invariant(&compressed_graph);
}

// ---------------------------------------------------------------------------------------------
// 5. Transactions
// ---------------------------------------------------------------------------------------------

#[test]
fn rollbacks_restore_the_logical_data_and_the_invariant() {
    let oracle = content(&compressed_graph());

    // A failing traversal rolls back its writes.
    let graph = shared(compressed_graph());
    let failed = eval_value(
        graph.clone(),
        &expand(
            r#"g.v("c0").property("bio", "x".repeat(900)).property("details", 1).values("name").as_number().to_list()"#,
        ),
    );
    assert!(failed.is_err());
    invariant(&graph.read());
    let Ok(graph) = Arc::try_unwrap(graph) else {
        panic!("the graph is still shared")
    };
    let mut graph = graph.into_inner();
    assert_eq!(content(&graph), oracle);

    // A transaction mixing writes and rule changes, rolled back.
    type Failure = Box<dyn std::error::Error>;
    let result: Result<(), Failure> = graph.transaction(|g| {
        g.traversal_mut()
            .v(vec!["c0", "c1"])
            .property("bio", "r".repeat(700).as_str())
            .to_list()?;
        g.set_definition(Definition::compression(
            "bio",
            rule(ElementKind::Vertex, "customer", "bio").with_min_bytes(5000),
        ))?;
        g.remove_definition(DefinitionKind::COMPRESSION, "notes")?;
        g.set_definition(Definition::compression(
            "city",
            rule(ElementKind::Vertex, "customer", "details.city").with_min_bytes(1),
        ))?;
        // A nested savepoint that is rolled back on its own.
        let inner: Result<(), Failure> = g.transaction(|g| {
            g.remove_definition(DefinitionKind::COMPRESSION, "career_cv")?;
            let c2 = g.get_vertex_by_id("c2").unwrap().unwrap();
            g.drop_vertex(c2)?;
            Err("inner".into())
        });
        assert!(inner.is_err());
        invariant(g);
        Err("outer".into())
    });
    assert!(result.is_err());
    invariant(&graph);
    assert_eq!(content(&graph), oracle);
    assert_eq!(graph.definitions().unwrap().count(), 4);
    assert!(compressed_count(&graph) > 10);

    // A dry run leaves the graph as it was and reports plain values.
    let (_, changes) = graph
        .dry_run(|g| {
            g.remove_definition(DefinitionKind::COMPRESSION, "bio")?;
            g.traversal_mut()
                .v(vec!["c0"])
                .property("bio", "d".repeat(800).as_str())
                .to_list()?;
            Ok::<(), Failure>(())
        })
        .unwrap();
    assert!(changes.mutations().len() >= 2);
    assert_plain_changes(changes.mutations());
    invariant(&graph);
    assert_eq!(content(&graph), oracle);
}

fn assert_plain_changes(mutations: &[Mutation]) {
    let text = format!("{mutations:?}");
    for mutation in mutations {
        match mutation {
            Mutation::SetProperty { before, after, .. } => {
                assert!(no_compressed_inside(after), "{text}");
                if let Some(before) = before {
                    assert!(no_compressed_inside(before), "{text}");
                }
            }
            Mutation::RemoveProperty { before, .. } => {
                assert!(no_compressed_inside(before), "{text}")
            }
            Mutation::AddVertex { properties, .. } | Mutation::AddEdge { properties, .. } => {
                assert!(properties.values().all(no_compressed_inside), "{text}")
            }
            Mutation::DropVertex { before, .. } => {
                assert!(
                    before.properties.values().all(no_compressed_inside),
                    "{text}"
                )
            }
            Mutation::DropEdge { before, .. } => {
                assert!(
                    before.properties.values().all(no_compressed_inside),
                    "{text}"
                )
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------
// 6. Change capture
// ---------------------------------------------------------------------------------------------

struct Recorder(Arc<Mutex<Vec<ChangeSet>>>);

impl CommitHook for Recorder {
    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        self.0.lock().unwrap().push(changes.clone());
        Ok(())
    }
}

#[test]
fn hooks_see_plain_values_and_rule_changes_only_as_definitions() {
    let mut graph = plain_graph();
    let seen = Arc::new(Mutex::new(Vec::new()));
    graph
        .register_commit_hook(Box::new(Recorder(seen.clone())))
        .unwrap();
    define_rules(&mut graph);
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        for changes in seen.iter() {
            assert_eq!(changes.mutations().len(), 1, "{changes:?}");
            assert!(matches!(
                changes.mutations()[0],
                Mutation::SetDefinition { .. }
            ));
        }
    }
    let seq = graph.last_commit_seq();
    graph.traversal_mut().recompress("bio").unwrap();
    assert_eq!(graph.last_commit_seq(), seq, "a recompress commits nothing");
    assert_eq!(seen.lock().unwrap().len(), 4);

    // Writes and drops of compressed values carry plain before/after values.
    graph
        .traversal_mut()
        .v(vec!["c0"])
        .property("bio", "p".repeat(900).as_str())
        .to_list()
        .unwrap();
    graph
        .traversal_mut()
        .v(vec!["c2"])
        .drop()
        .to_list()
        .unwrap();
    graph
        .traversal_mut()
        .v(vec!["c3"])
        .remove_property(vec!["bio"])
        .to_list()
        .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 7);
    for changes in seen.iter() {
        assert_plain_changes(changes.mutations());
    }
    // The serialized change set holds the plain text.
    #[cfg(feature = "serde")]
    {
        let json = serde_json::to_string(&seen[4]).unwrap();
        assert!(json.contains(&cv(0, 300)), "{json}");
    }
}

#[test]
fn a_replayed_journal_compresses_on_the_replica() {
    let mut live = TraversalGraph::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    live.register_commit_hook(Box::new(Recorder(seen.clone())))
        .unwrap();
    populate(&mut live);
    define_rules(&mut live);
    live.traversal_mut()
        .v(vec!["c0"])
        .property("bio", "j".repeat(800).as_str())
        .to_list()
        .unwrap();
    let mut replica = TraversalGraph::new();
    // `populate` ran outside units (no change sets): rebuild it, then replay the commits.
    populate(&mut replica);
    for changes in seen.lock().unwrap().iter() {
        replica = replica.replay(changes).unwrap();
    }
    invariant(&replica);
    assert_eq!(content(&replica), content(&live));
    assert_eq!(compressed_count(&replica), compressed_count(&live));
    // apply_change_set on a live graph too.
    let mut target = TraversalGraph::new();
    populate(&mut target);
    for changes in seen.lock().unwrap().iter() {
        target.apply_change_set(changes).unwrap();
    }
    invariant(&target);
    assert_eq!(content(&target), content(&live));
}

// ---------------------------------------------------------------------------------------------
// 7. Validation
// ---------------------------------------------------------------------------------------------

#[test]
fn invalid_rules_are_rejected_and_change_nothing() {
    let mut graph = compressed_graph();
    let before = (content(&graph), graph.definitions().unwrap().count());
    assert!(CompressionDefinition::new(ElementKind::Vertex, "customer", "details[0]").is_err());
    assert!(CompressionDefinition::new(ElementKind::Vertex, "customer", "list[-1]").is_err());
    assert!(CompressionDefinition::new(ElementKind::Vertex, "customer", "").is_err());
    let bad = [
        Definition::compression(
            "zero",
            rule(ElementKind::Vertex, "customer", "x").with_min_bytes(0),
        ),
        Definition::compression(
            "huge",
            rule(ElementKind::Vertex, "customer", "x").with_min_bytes(MAX_MIN_BYTES + 1),
        ),
        Definition::compression("dup", rule(ElementKind::Vertex, "customer", "bio")),
        Definition::compression("bad name", rule(ElementKind::Vertex, "customer", "y")),
        Definition::compression("label", rule(ElementKind::Vertex, "", "y")),
    ];
    for definition in bad {
        let err = graph
            .traversal_mut()
            .set_definition(definition.clone())
            .unwrap_err();
        let text = err.to_string();
        assert!(!text.is_empty(), "{definition:?}");
        assert_eq!(
            (content(&graph), graph.definitions().unwrap().count()),
            before,
            "{definition:?}"
        );
        invariant(&graph);
    }
    let err = graph
        .traversal_mut()
        .set_definition(Definition::compression(
            "dup",
            rule(ElementKind::Vertex, "customer", "bio"),
        ))
        .unwrap_err();
    let diagnostic = graphersal::error::format_diagnostic(&err);
    assert!(diagnostic.contains("already covers"), "{diagnostic}");
    assert!(diagnostic.contains("drop_compression"), "{diagnostic}");

    // The DSL: unknown codec, index paths, bad keys.
    let shared = shared(compressed_graph());
    for script in [
        r#"g.define_compression(#{name: "z", label: "customer", path: "bio2", codec: "zip"})"#,
        r#"g.define_compression(#{name: "z", label: "customer", path: "a[1]"})"#,
        r#"g.define_compression(#{name: "z", label: "customer", path: jpath("a[-1]")})"#,
        r#"g.define_compression(#{name: "z", label: "customer"})"#,
        r#"g.define_compression(#{name: "z", label: "customer", path: "x", colour: 1})"#,
        r#"g.define_compression(#{name: "z", label: "customer", path: "x", min_bytes: 0})"#,
        r#"g.define_compression(#{name: "if", label: "customer", path: "x"})"#,
        r#"g.recompress("nope")"#,
    ] {
        let err = eval_value(shared.clone(), script).unwrap_err().to_string();
        assert!(err.contains("Help:") || !err.is_empty(), "{script}: {err}");
    }
    assert_eq!(shared.read().definitions().unwrap().count(), 4);
    // The defaults.
    let r = CompressionDefinition::new(ElementKind::Vertex, "a", "b").unwrap();
    assert_eq!(r.min_bytes(), DEFAULT_MIN_BYTES);
    assert!(r.dictionary());
    assert_eq!(r.codec(), Codec::Lz4);
}

#[test]
fn the_exchange_form_round_trips() {
    let definition = Definition::compression(
        "cv",
        rule(ElementKind::Edge, "bought", "a['b c'].d")
            .with_min_bytes(99)
            .with_dictionary(false),
    );
    let json = definition.to_json();
    assert!(json.contains("\"kind\":\"compression\""), "{json}");
    assert_eq!(Definition::from_json(&json).unwrap(), definition);
    let catalog = graphersal::catalog::catalog_to_json([&definition]);
    assert_eq!(
        graphersal::catalog::catalog_from_json(&catalog).unwrap(),
        vec![definition]
    );
}

// ---------------------------------------------------------------------------------------------
// 8. Persistence
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "persist")]
#[test]
fn a_snapshot_holds_plain_text_and_a_load_compresses_again() {
    use graphersal::persist::{Codec as ChunkCodec, WriteOptions, write_snapshot};
    let graph = compressed_graph();
    let mut bytes = Vec::new();
    write_snapshot(
        &graph,
        &mut bytes,
        &WriteOptions::new().with_codec(ChunkCodec::None),
    )
    .unwrap();
    let needle = cv(0, 300);
    assert!(
        bytes.windows(needle.len()).any(|w| w == needle.as_bytes()),
        "the plain text is on disk"
    );
    let loaded = TraversalGraph::read_snapshot(bytes.as_slice()).unwrap();
    invariant(&loaded);
    assert_eq!(content(&loaded), content(&graph));
    assert_eq!(compressed_count(&loaded), compressed_count(&graph));
    assert!(loaded.compression_stats("bio").unwrap().dictionary_bytes > 0);
}

// ---------------------------------------------------------------------------------------------
// 9. Exchange formats
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "io")]
#[test]
fn graphson_and_graphml_exports_equal_the_exports_without_rules() {
    use graphersal::io::GraphImport;

    let plain = plain_graph();
    let compressed = compressed_graph();
    let export = |graph: &TraversalGraph| {
        let mut graphson = Vec::new();
        graph
            .traversal()
            .export_graphson_writer(&mut graphson)
            .unwrap();
        let mut graphml = Vec::new();
        graph
            .traversal()
            .export_graphml_writer(&mut graphml)
            .unwrap();
        (graphson, graphml)
    };
    let (son_plain, ml_plain) = export(&plain);
    let (son, ml) = export(&compressed);
    assert_eq!(son, son_plain);
    assert_eq!(ml, ml_plain);

    // An import into a graph with rules compresses.
    let mut target = TraversalGraph::new();
    define_rules(&mut target);
    target.import_graphson_reader(son.as_slice()).unwrap();
    invariant(&target);
    assert_eq!(content(&target), content(&plain));
    assert_eq!(compressed_count(&target), compressed_count(&compressed));
    let mut target = TraversalGraph::new();
    define_rules(&mut target);
    target.import_graphml_reader(ml.as_slice()).unwrap();
    invariant(&target);
    assert!(compressed_count(&target) > 10);
}

// ---------------------------------------------------------------------------------------------
// 10. Schema
// ---------------------------------------------------------------------------------------------

#[test]
fn schema_scans_and_writes_see_the_text() {
    let mut graph = compressed_graph();
    let schema = |max_length: u64, pattern: &str| {
        GraphSchema::from_json(&format!(
            r#"{{"mode": "open", "vertices": {{"customer": {{"schema": {{"type": "object",
                "properties": {{"bio": {{"type": "string", "maxLength": {max_length},
                "pattern": "{pattern}"}}}}}}}}}}}}"#
        ))
        .unwrap()
    };
    // maxLength too small: the compressed values violate it.
    let err = graph.set_schema(schema(100, ".*")).unwrap_err();
    assert!(err.to_string().contains("bio"), "{err}");
    // A pattern the CV texts do not match.
    let err = graph.set_schema(schema(100_000, "^XYZ")).unwrap_err();
    assert!(err.to_string().contains("bio"), "{err}");
    // A schema the data satisfies.
    graph.set_schema(schema(100_000, "^(CV|short)")).unwrap();
    invariant(&graph);
    // A write is still validated and compressed.
    let err = graph
        .traversal_mut()
        .v(vec!["c0"])
        .property("bio", "XYZ".repeat(200).as_str())
        .to_list()
        .unwrap_err();
    let diagnostic = graphersal::error::format_diagnostic(&err);
    assert!(diagnostic.contains("pattern"), "{diagnostic}");
    graph
        .traversal_mut()
        .v(vec!["c0"])
        .property("bio", format!("CV {}", "a".repeat(600)).as_str())
        .to_list()
        .unwrap();
    invariant(&graph);
    assert!(matches!(
        graph
            .vertex_properties(graph.get_vertex_by_id("c0").unwrap().unwrap())
            .unwrap()
            .unwrap()
            .get("bio"),
        Some(ElementProperty::Compressed(_))
    ));
    // Inference reports a string.
    let inferred = graph.traversal().infer_schema().unwrap().to_json();
    assert!(
        inferred.contains("\"bio\":{\"type\":\"string\"}"),
        "{inferred}"
    );
}

// ---------------------------------------------------------------------------------------------
// 11. Permissions
// ---------------------------------------------------------------------------------------------

#[test]
fn read_only_policies_and_saved_queries_cannot_change_rules() {
    let mut graph = compressed_graph();
    let policy: Arc<dyn graphersal::auth::Authorizer> = Arc::new(AccessPolicy::read_only());
    let mut g = graph.traversal_mut().with_authorizer(policy.clone());
    assert!(
        g.define_compression("x", rule(ElementKind::Vertex, "customer", "x"))
            .is_err()
    );
    assert!(
        g.remove_definition(DefinitionKind::COMPRESSION, "bio")
            .is_err()
    );
    assert!(g.recompress("bio").is_err());
    let listed = g
        .list_definitions(Some(DefinitionKind::COMPRESSION))
        .unwrap();
    assert_eq!(listed.len(), 4);
    assert!(g.compression_stats("bio").unwrap().is_some());

    // The DSL under the read-only policy and inside a saved query.
    let shared = shared(compressed_graph());
    let engine = graphersal::script::engine_with_limits(&ScriptLimits::default(), policy.clone());
    for script in [
        r#"g.define_compression(#{name: "x", label: "customer", path: "x"})"#,
        r#"g.drop_compression("bio")"#,
        r#"g.recompress("bio")"#,
    ] {
        let mut scope =
            graphersal::script::graph_scope_with_options(shared.clone(), &[], policy.clone());
        let err = engine
            .eval_with_scope::<graphersal::script::rhai::Dynamic>(&mut scope, script)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("ermission") || err.contains("denied"),
            "{script}: {err}"
        );
    }
    let mut scope =
        graphersal::script::graph_scope_with_options(shared.clone(), &[], policy.clone());
    let listed = engine
        .eval_with_scope::<graphersal::script::rhai::Dynamic>(&mut scope, "g.compressions().len()")
        .unwrap();
    assert_eq!(listed.as_int().unwrap(), 4);

    // Stored through the Rust API in the read-only dialect (as by 0.2.0 or a catalog file):
    // `g.define_query` refuses such a body (it changes the catalog).
    shared
        .write()
        .traversal_mut()
        .define_query(
            "sneaky",
            graphersal::catalog::QueryDefinition::new(
                r#"fn sneaky() { g.drop_compression("bio") }"#,
            ),
        )
        .unwrap();
    let err = eval_value(shared.clone(), r#"g.query("sneaky")"#)
        .unwrap_err()
        .to_string();
    assert!(err.contains("read-only"), "{err}");
    assert_eq!(shared.read().definitions().unwrap().count(), 5);
}

// ---------------------------------------------------------------------------------------------
// 12. Randomized differential test
// ---------------------------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Applies one random operation to `graph` (with rules: `rules` true) or to the oracle.
fn random_op(graph: &mut TraversalGraph, op: usize, arg: usize, rules: bool) {
    let id = format!("c{}", arg % 12);
    let Some(v) = graph.get_vertex_by_id(&id).unwrap() else {
        return;
    };
    match op {
        0 => {
            let _ = graph.set_vertex_property(v, "bio".into(), cv(arg, 100 + arg * 13).into());
        }
        1 => {
            let _ = graph.set_vertex_property(v, "bio".into(), "s".into());
        }
        2 => {
            let path: JsonPath = "details.career.cv".parse().unwrap();
            let _ = graph.set_vertex_property_path(v, &path, cv(arg, 300 + arg * 7).into());
        }
        3 => {
            let _ = graph.drop_vertex_property("bio", v);
        }
        4 => {
            let _ = graph.add_vertex_label(v, "vip".into());
        }
        5 => {
            let _ = graph.remove_vertex_label(v, "vip");
        }
        6 => {
            let _ = graph.set_vertex_property(
                v,
                "details".into(),
                object(vec![("career", object(vec![("cv", cv(arg, 900).into())]))]),
            );
        }
        7 if rules => {
            let min = [16u32, 256, 1024][arg % 3];
            let _ = graph.set_definition(Definition::compression(
                "bio",
                rule(ElementKind::Vertex, "customer", "bio")
                    .with_min_bytes(min)
                    .with_dictionary(arg.is_multiple_of(2)),
            ));
        }
        8 if rules => {
            let _ = graph.remove_definition(DefinitionKind::COMPRESSION, "career_cv");
        }
        9 if rules => {
            let _ = graph.set_definition(Definition::compression(
                "career_cv",
                rule(ElementKind::Vertex, "customer", "details.career.cv"),
            ));
        }
        10 if rules => {
            let _ = graph.traversal_mut().recompress("bio");
        }
        11 => {
            let _ = graph.set_vertex_property(v, "summary".into(), cv(arg, 450).into());
        }
        _ => {}
    }
}

#[test]
fn random_histories_keep_the_invariant_and_the_logical_data() {
    let cases: usize = std::env::var("COMPRESSION_FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12);
    for seed in 1..=cases as u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut graph = compressed_graph();
        let mut oracle = plain_graph();
        for _ in 0..40 {
            // A unit of a few operations, committed or rolled back.
            let ops: Vec<(usize, usize)> = (0..1 + rng.below(4))
                .map(|_| (rng.below(12), rng.below(1000)))
                .collect();
            let commit = rng.below(3) != 0;
            let mark = graph.begin_unit().unwrap();
            for &(op, arg) in &ops {
                random_op(&mut graph, op, arg, true);
            }
            if commit {
                graph.commit_unit(mark).unwrap();
                for &(op, arg) in &ops {
                    random_op(&mut oracle, op, arg, false);
                }
            } else {
                graph
                    .rollback_unit(
                        mark,
                        &graphersal::changes::RollbackReason::failed(&"random rollback"),
                    )
                    .unwrap();
            }
            if let Err(reason) = graph.check_compression_invariant() {
                panic!("seed {seed}: {reason}");
            }
            assert_eq!(content(&graph), content(&oracle), "seed {seed}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// 13. The DSL in both spellings
// ---------------------------------------------------------------------------------------------

#[test]
fn the_dsl_works_in_both_spellings() {
    let graph = shared(plain_graph());
    let run = |script: &str| eval_value(graph.clone(), script).unwrap();
    let _ = run(
        r#"g.define_compression(#{name: "bio", label: "customer", path: "bio", min_bytes: 128})"#,
    );
    let _ = run(
        r#"g.define_compression(#{name: "notes", element: "edge", label: "bought", path: jpath("$.notes"), codec: "lz4", dictionary: false})"#,
    );
    let listed = run("g.compressions()").to_string();
    assert!(listed.contains("\"$.notes\""), "{listed}");
    assert!(listed.contains("compressed_values"), "{listed}");
    let values = run(r#"g.compressions().map(|r| r.compressed_values)"#).to_string();
    assert!(!values.contains("[0, 0]"), "{values}");
    let _ = run(r#"g.recompress("bio")"#);
    assert!(run(r#"g.drop_compression("notes")"#).as_bool().unwrap());
    assert!(!run(r#"g.drop_compression("notes")"#).as_bool().unwrap());
    assert_eq!(run("g.compressions().len()").as_int().unwrap(), 1);
    let memory = run("g.memory_usage()").to_string();
    assert!(memory.contains("compressed_values"), "{memory}");
    invariant(&graph.read());
}

// ---------------------------------------------------------------------------------------------
// 14. Measurement (run explicitly: `--release -- --ignored --nocapture`)
// ---------------------------------------------------------------------------------------------

#[test]
#[ignore = "a measurement; run with --release -- --ignored --nocapture"]
fn measure_ratio_and_throughput() {
    let corpus: Vec<String> = (0..5_000)
        .map(|i| realistic_cv(i as u64, 2_000 + (i * 997) % 18_000))
        .collect();
    let plain_bytes: usize = corpus.iter().map(String::len).sum();
    for dictionary in [false, true] {
        let mut graph = TraversalGraph::new();
        for text in &corpus {
            graph
                .add_vertex(
                    None,
                    vec!["person".into()],
                    props(vec![("cv", text.as_str().into())]),
                )
                .unwrap();
        }
        // The explicit pass of a define: builds the dictionary and compresses every value.
        let start = std::time::Instant::now();
        graph
            .traversal_mut()
            .define_compression(
                "cv",
                rule(ElementKind::Vertex, "person", "cv").with_dictionary(dictionary),
            )
            .unwrap();
        let compress_secs = start.elapsed().as_secs_f64();
        let stats = graph.compression_stats("cv").unwrap();
        let start = std::time::Instant::now();
        let mut read = 0usize;
        for _ in 0..5 {
            for value in graph.traversal().v(None).values("cv").to_list().unwrap() {
                read += value.as_str().map_or(0, str::len);
            }
        }
        let read_secs = start.elapsed().as_secs_f64();
        let memory = graph.memory_usage().unwrap().unwrap();
        println!(
            "dictionary={dictionary}: {} of {} values compressed, plain {} bytes, stored {} bytes \
             (ratio {:.3}, dictionary {} bytes); property_bytes {}; define pass (compress) \
             {:.0} MB/s; read (decompress + materialize) {:.0} MB/s",
            stats.compressed_values,
            corpus.len(),
            plain_bytes,
            stats.stored_bytes,
            stats.stored_bytes as f64 / plain_bytes as f64,
            stats.dictionary_bytes,
            memory.property_bytes,
            plain_bytes as f64 / compress_secs / 1e6,
            read as f64 / read_secs / 1e6,
        );
    }
}

/// A CV-like text of about `len` bytes with realistic variety: section headers, sentences of
/// random words from a 200-word vocabulary, names, companies, years and numbers (so LZ4 does
/// not find the long repeats the battery's fixture has).
fn realistic_cv(seed: u64, len: usize) -> String {
    const WORDS: &str = "the a and of to in for with on as by at from team teams lead led \
        built designed developed managed delivered improved reduced increased migrated \
        introduced owned scaled maintained mentored hired reviewed shipped launched automated \
        platform service services system systems data pipeline pipelines graph database \
        databases cloud infrastructure api apis backend frontend mobile web product products \
        customer customers users revenue cost latency throughput availability reliability \
        security compliance performance quality testing release releases deployment \
        deployments monitoring incident incidents on-call architecture design code review \
        open source community conference talk talks paper papers patent research model \
        models machine learning analytics reporting dashboard dashboards search ranking \
        recommendation payments billing checkout logistics warehouse inventory supply chain \
        retail banking insurance healthcare telecom energy automotive gaming media \
        streaming video audio messaging chat email notification notifications batch \
        realtime event events queue queues cache caching storage index indexes query \
        queries schema migration migrations rust go java kotlin python typescript scala \
        kubernetes docker terraform aws gcp azure postgres mysql kafka redis elasticsearch \
        spark flink airflow grpc rest graphql react vue angular linux networking agile \
        scrum roadmap strategy budget stakeholders partners vendors hiring onboarding \
        coaching career growth culture remote distributed global regional local";
    const NAMES: [&str; 16] = [
        "Acme",
        "Globex",
        "Initech",
        "Umbrella",
        "Hooli",
        "Vandelay",
        "Stark",
        "Wayne",
        "Tyrell",
        "Cyberdyne",
        "Soylent",
        "Wonka",
        "Gringotts",
        "Aperture",
        "Massive",
        "Dunder",
    ];
    const SECTIONS: [&str; 6] = [
        "Experience",
        "Education",
        "Skills",
        "Projects",
        "Publications",
        "Languages",
    ];
    let words: Vec<&str> = WORDS.split_whitespace().collect();
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut out = format!("Curriculum vitae #{seed}\n");
    let mut section = 0;
    while out.len() < len {
        if next() % 9 == 0 {
            out.push('\n');
            out.push_str(SECTIONS[section % SECTIONS.len()]);
            out.push_str(":\n");
            section += 1;
        }
        let year = 1995 + next() % 30;
        out.push_str(&format!(
            "{}-{}: {} {} ",
            year,
            year + 1 + next() % 6,
            NAMES[(next() % 16) as usize],
            ["GmbH", "Inc.", "Ltd", "AG", "LLC"][(next() % 5) as usize]
        ));
        let sentence_words = 8 + next() % 14;
        for k in 0..sentence_words {
            let word = words[(next() % words.len() as u64) as usize];
            if k == 0 {
                let mut chars = word.chars();
                if let Some(first) = chars.next() {
                    out.extend(first.to_uppercase());
                    out.push_str(chars.as_str());
                }
            } else {
                out.push_str(word);
            }
            if next() % 11 == 0 {
                out.push_str(&format!(" {}%", next() % 100));
            }
            out.push(' ');
        }
        out.pop();
        out.push_str(". ");
    }
    out.truncate(len);
    out
}

#[test]
fn rules_survive_the_catalog_listing_helpers() {
    let graph = compressed_graph();
    let rules: HashMap<String, CompressionDefinition> = graph
        .traversal()
        .list_definitions(Some(DefinitionKind::COMPRESSION))
        .unwrap()
        .into_iter()
        .map(|d| (d.name().to_string(), d.as_compression().unwrap().clone()))
        .collect();
    assert_eq!(rules.len(), 4);
    assert_eq!(rules["notes"].element(), ElementKind::Edge);
    assert_eq!(rules["career_cv"].path_text(), "$.details.career.cv");
    assert_eq!(rules["career_cv"].min_bytes(), 512);
}
