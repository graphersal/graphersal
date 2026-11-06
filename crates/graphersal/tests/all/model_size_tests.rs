//! The size a model shows and the room its load asks for (user decision 2026-11-04, book page
//! "Embedding Models in a Store"): a model that is not loaded shows its size on disk (its blob),
//! a loaded one the memory measured at its load; nothing about the size is read from the
//! definition. The load streams the blob (no whole-tar copy): the vectors are the same, and a
//! damaged blob is still damage.
#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]

use graphersal::persist::{ModelState, Store};
#[allow(unused_imports)]
use graphersal::prelude::*;

use crate::model_fixture::*;

#[test]
fn a_model_that_is_not_loaded_shows_its_size_on_disk() {
    let temp = Temp::new("size-unloaded");
    let store = Store::create(temp.join("s"), options()).unwrap();
    import(&store, 1, "acme/tiny");
    let models = store.models().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].state, ModelState::NotLoaded);
    assert_eq!(models[0].memory_bytes, None);
    assert_eq!(models[0].shown_bytes(), models[0].bytes);
    assert!(models[0].bytes > 0);
}

/// Loads (feature `model2vec`).
#[cfg(feature = "model2vec")]
mod loaded {
    use super::*;

    #[test]
    fn a_loaded_model_shows_the_memory_measured_at_its_load() {
        let temp = Temp::new("size-loaded");
        let path = temp.join("s");
        let identity = {
            let store = Store::create(&path, options()).unwrap();
            let model = import(&store, 1, "acme/tiny").model;
            set_schema(&store, &model.identity);
            store.close().unwrap();
            model.identity
        };
        let store = Store::open(&path, options()).unwrap();
        let models = store.models().unwrap();
        assert_eq!(models[0].state, ModelState::Loaded);
        let measured: Vec<_> =
            graphersal::storage::AnyGraph::process_memory_components(&**store.graph())
                .unwrap()
                .into_iter()
                .filter(|part| part.name == "embedding_model")
                // The registry is process-wide: models other tests load at the same time are
                // listed too, so only this test's model counts.
                .filter(|part| part.detail.as_deref() == Some(identity.as_str()))
                .collect();
        assert_eq!(measured.len(), 1);
        assert_eq!(measured[0].detail.as_deref(), Some(identity.as_str()));
        assert_eq!(models[0].memory_bytes, Some(measured[0].bytes));
        assert_eq!(models[0].shown_bytes(), measured[0].bytes);
        assert_ne!(models[0].shown_bytes(), models[0].bytes);
    }

    #[test]
    fn a_lazily_loaded_model_embeds_like_one_loaded_at_open() {
        // The same text through a model the open loads and through one a query loads lazily:
        // both read the blob as a stream.
        let temp = Temp::new("size-stream");
        let lazy = Store::create(temp.join("lazy"), options()).unwrap();
        import(&lazy, 2, "acme/tiny");
        lazy.set_default_model(Some("acme/tiny")).unwrap();
        let vector = |store: &Store| {
            store
                .graph()
                .read()
                .traversal()
                .inject(vec!["graph vector model"])
                .embed_value(Some("acme/tiny"))
                .to_list()
                .unwrap()
        };
        let lazily = vector(&lazy);
        assert_eq!(lazy.models().unwrap()[0].state, ModelState::Loaded);
        lazy.close().unwrap();
        let path = temp.join("eager");
        {
            let store = Store::create(&path, options()).unwrap();
            let model = import(&store, 2, "acme/tiny").model;
            set_schema(&store, &model.identity);
            store.close().unwrap();
        }
        let eager = Store::open(&path, options()).unwrap();
        assert_eq!(vector(&eager), lazily);
    }
}
