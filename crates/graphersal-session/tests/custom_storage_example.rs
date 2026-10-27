//! The complete host example of the book page `for_developers/custom_storage.md` (included
//! there between the `ANCHOR` lines, so the page shows exactly what is compiled and run here).
//!
//! A host brings its own storage, here the `Forwarding` proof storage of
//! `graphersal-storage-tests` (its own handle types, every method forwarded to a
//! `TraversalGraph`), and plugs it into every front end that is generic: the engine, the Rhai DSL
//! with saved queries, a Store and a session serving that Store.

// ANCHOR: example
use std::sync::Arc;

use graphersal::TraversalGraph;
use graphersal::persist::{MemDir, Store, StoreDir, StoreOptions};
use graphersal::prelude::{ElementProperty, GraphTraversalSource};
use graphersal::script::eval_value;
use graphersal::storage::{Graph, GraphStorage, Transactional};
use graphersal_session::{RunOptions, Session, StorageKind};
use graphersal_storage_tests::forwarding::Forwarding;

/// The host's storage. A real host writes its own `GraphStorage` (+ `Default`, and
/// `PersistentStorage` to be stored); `Forwarding` stands in for it.
type MyStorage = Forwarding;

fn modern() -> MyStorage {
    Forwarding::new(TraversalGraph::tinkerpop_modern())
}

#[test]
fn a_host_plugs_its_own_storage_into_every_front_end() -> Result<(), Box<dyn std::error::Error>> {
    // 1. The engine is generic: a traversal over `&S` (reads) or `&mut S` (writes).
    let mut storage = modern();
    let people = GraphTraversalSource::new(&storage)
        .v(None)
        .has_label("person")
        .count()
        .next()?;
    assert_eq!(people, Some(ElementProperty::from(4)));
    storage.transaction(|g| {
        GraphTraversalSource::new(&mut *g).add_v("robot").next()?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;
    assert_eq!(storage.vertex_count(), 7);

    // 2. The DSL and saved queries take any `Arc<Graph<S>>` (`impl IntoAnyGraph`).
    //    `new_persistent` also gives the DSL's `g.export_snapshot(path)`.
    let graph = Arc::new(Graph::new_persistent(modern()));
    let _definition = eval_value(
        graph.clone(),
        r#"g.define_query(#{name: "older_than", params: #{age: "integer"},
               body: "fn older_than(age) { g.V().has(\"age\", P.gt(age)).values(\"name\").order() }"})"#,
    )?;
    let names = eval_value(
        graph.clone(),
        r#"g.query("older_than", #{age: 30}).toList()"#,
    )?;
    assert_eq!(names.to_string(), r#"["josh", "peter"]"#);

    // 3. A Store of the storage: `create_in` / `open_in` (it must claim `atomic` and
    //    `change_capture`). Every commit goes to the write-ahead log.
    let dir: Arc<dyn StoreDir> = Arc::new(MemDir::named("custom-storage-example"));
    let store = Store::create_in(&dir, modern(), StoreOptions::new())?;
    store.graph().write().transaction(|g| {
        GraphTraversalSource::new(&mut *g)
            .add_v("person")
            .property("name", "ada")
            .next()?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;
    store.close()?;
    // `open_in` takes how to build the empty instance the snapshot is loaded into.
    let store: Store<MyStorage> = Store::open_in(&dir, StoreOptions::new(), MyStorage::default)?;
    assert_eq!(store.graph().read().vertex_count(), 7);

    // 4. A session over the storage: every graph it builds (samples, loads, stores in memory,
    //    forks) is a `MyStorage`; here it serves the Store opened above.
    let mut session = Session::with_storage(Arc::new(StorageKind::<MyStorage>::persistent()));
    session.attach_store(Arc::new(store), "my store")?;
    let answer = session.execute(
        r#"g.V().has("name", "ada").count().next()"#,
        &RunOptions::default(),
    );
    assert!(answer.contains(r#""ok":true"#), "{answer}");
    assert_eq!(
        session.graph().storage_type_name(),
        std::any::type_name::<MyStorage>()
    );
    let mark = session.store_mark("after-ada")?;
    assert_eq!(mark["mark"]["name"], "after-ada");
    Ok(())
}
// ANCHOR_END: example
