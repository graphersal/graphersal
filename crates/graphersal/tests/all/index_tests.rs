//! Integration tests for primary graph index and GraphML import performance/correctness via public API.

use graphersal::prelude::*;

#[test]
fn test_index_uniqueness_and_lifecycle_via_traversal() {
    let graph = GraphSource::empty();

    let v1 = graph
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", "Alice")
        .next()
        .expect("should create vertex 1")
        .expect("should return element property");

    let v2 = graph
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", "Bob")
        .next()
        .expect("should create vertex 2")
        .expect("should return element property");

    assert!(matches!(v1, ElementProperty::Vertex(_)));
    assert!(matches!(v2, ElementProperty::Vertex(_)));

    let count = graph
        .read()
        .traversal()
        .v(None::<()>)
        .count()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(count.as_i64(), Some(2));

    // Verify vertices_by_label on underlying TraversalGraph via GraphStorage
    {
        let guard = graph.read();
        let storage = &*guard;
        let persons: Vec<_> = storage.vertices_by_label("person").unwrap().collect();
        assert_eq!(persons.len(), 2);
        let unknown: Vec<_> = storage.vertices_by_label("software").unwrap().collect();
        assert!(unknown.is_empty());
    }
}

/// Correctness check for the `Vec` → `IndexSet` migration of the label index's buckets
/// (`node_labels`/`edge_labels`, see `graph/index.rs`): dropping a large fraction of a large
/// label's members must leave the bucket with exactly the right remaining set — no over-removal
/// (a stray `swap_remove` taking out the wrong element), no stale entries (a removed vertex's
/// index lingering in the bucket), and no under-removal. This is a correctness test, independent
/// of the algorithmic-complexity fix's performance benefit).
#[test]
fn test_dropping_a_large_fraction_of_a_large_label_bucket_leaves_the_exact_remainder() {
    use std::collections::HashSet;

    let graph = GraphSource::empty();
    const TOTAL: i64 = 300;
    const DROP_COUNT: i64 = 200;

    // Populate a large "big" label bucket (300 members) plus an untouched "small" bucket (10
    // members), so we can also confirm the drop doesn't disturb an unrelated label's bucket.
    {
        let mut guard = graph.write();
        let mut trav = guard.traversal_mut();
        for i in 0..TOTAL {
            trav = trav.add_v("big").property("idx", i);
        }
        for i in 0..10 {
            trav = trav.add_v("small").property("idx", i);
        }
        let _ = trav.to_list().expect("population should succeed");
    }

    // Capture the bucket's full member order before any removal, so we know exactly which
    // `idx` values `limit(DROP_COUNT)` will select for drop (IndexSet preserves insertion
    // order, same as the old Vec did).
    let all_idx_before: Vec<i64> = graph
        .read()
        .traversal()
        .v(None::<()>)
        .has_label("big")
        .values("idx")
        .to_list()
        .expect("read before drop should succeed")
        .into_iter()
        .map(|v| v.as_i64().expect("idx should be a number"))
        .collect();
    assert_eq!(all_idx_before.len(), TOTAL as usize);

    let expected_dropped: HashSet<i64> = all_idx_before[..DROP_COUNT as usize]
        .iter()
        .copied()
        .collect();
    let expected_remaining: HashSet<i64> = all_idx_before[DROP_COUNT as usize..]
        .iter()
        .copied()
        .collect();
    assert_eq!(expected_dropped.len(), DROP_COUNT as usize);
    assert_eq!(expected_remaining.len(), (TOTAL - DROP_COUNT) as usize);

    // Drop the first DROP_COUNT members of the "big" bucket.
    graph
        .write()
        .traversal_mut()
        .v(None::<()>)
        .has_label("big")
        .limit(DROP_COUNT as usize)
        .drop()
        .to_list()
        .expect("drop should succeed");

    // No under-removal, no over-removal: exactly TOTAL - DROP_COUNT members remain.
    let remaining_idx: HashSet<i64> = graph
        .read()
        .traversal()
        .v(None::<()>)
        .has_label("big")
        .values("idx")
        .to_list()
        .expect("read after drop should succeed")
        .into_iter()
        .map(|v| v.as_i64().expect("idx should be a number"))
        .collect();
    assert_eq!(remaining_idx.len(), (TOTAL - DROP_COUNT) as usize);

    // No stale entries (a dropped vertex still present) and the exact right set remains.
    assert_eq!(remaining_idx, expected_remaining);
    assert!(remaining_idx.is_disjoint(&expected_dropped));

    // The unrelated "small" label bucket is untouched.
    let small_count = graph
        .read()
        .traversal()
        .v(None::<()>)
        .has_label("small")
        .count()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(small_count.as_i64(), Some(10));
}
