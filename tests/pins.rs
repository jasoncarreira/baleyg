mod common;
use baleyg::{
    indexer::{IndexOptions, index_workspace},
    model::{CancelFlag, IndexPin, ViewQuery},
    store::Store,
};
use serde_json::json;
use std::sync::{Arc, atomic::AtomicBool};

fn fixture() -> (tempfile::TempDir, Store, baleyg::model::Graph, String) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("Types.java"),
        "class A { void run() { save(); } }",
    )
    .unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let graph = index_workspace(&IndexOptions::new(root.clone()), &cancel, |_| {}).unwrap();
    let id = graph
        .nodes
        .iter()
        .find(|node| node.name == "run")
        .unwrap()
        .id
        .clone();
    let store = crate::common::open_store(&temp.path().join("state"), &root).unwrap();
    (temp, store, graph, id)
}
fn publish(store: &Store, graph: &baleyg::model::Graph, expected: IndexPin) -> IndexPin {
    store
        .publish(
            graph,
            &store.leader().unwrap(),
            expected,
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap()
}
#[test]
fn pin_shape_and_status() {
    let (_temp, store, graph, _id) = fixture();
    let first = store.status().unwrap().revision;
    assert_eq!(first.index_revision, 0);
    assert_eq!(first.index_generation.get_version_num(), 4);
    let wire = serde_json::to_value(first).unwrap();
    assert_eq!(
        wire,
        json!({"indexGeneration":first.index_generation.to_string(),"indexRevision":0})
    );
    assert_eq!(
        serde_json::from_value::<IndexPin>(wire.clone()).unwrap(),
        first
    );
    for malformed in [
        json!(0),
        json!(null),
        json!({"indexRevision":0}),
        json!({"indexGeneration":first.index_generation.to_string()}),
        json!({"indexGeneration":first.index_generation.to_string(),"indexRevision":-1}),
        json!({"indexGeneration":first.index_generation.to_string(),"indexRevision":9007199254740992_u64}),
        json!({"indexGeneration":first.index_generation.to_string(),"indexRevision":0,"extra":true}),
        json!({"indexGeneration":first.index_generation.to_string().to_uppercase(),"indexRevision":0}),
        json!({"indexGeneration":uuid::Uuid::nil().to_string(),"indexRevision":0}),
    ] {
        assert!(
            serde_json::from_value::<IndexPin>(malformed.clone()).is_err(),
            "{malformed}"
        );
    }
    let next = publish(&store, &graph, first);
    assert_eq!(next.index_generation, first.index_generation);
    assert_eq!(next.index_revision, 1);
    assert_eq!(store.status().unwrap().revision, next);
}
#[test]
fn store_pinned_read_matrix() {
    let (_temp, store, graph, id) = fixture();
    let first = store.status().unwrap().revision;
    let pin = publish(&store, &graph, first);
    let stale = IndexPin {
        index_generation: pin.index_generation,
        index_revision: 0,
    };
    let fake = IndexPin {
        index_generation: uuid::Uuid::new_v4(),
        index_revision: 1,
    };
    for wrong in [stale, fake] {
        assert!(store.source_at("Types.java", Some(wrong)).is_err());
        assert!(store.source_at("missing", Some(wrong)).is_err());
        assert!(store.symbol_at(&id, Some(wrong)).is_err());
        assert!(store.symbol_at("missing", Some(wrong)).is_err());
        assert!(store.classes_at(None, "", Some(wrong), 0, 10).is_err());
        assert!(store.files_at(Some(wrong), 0, 10).is_err());
        assert!(store.methods_at("Types.java", Some(wrong)).is_err());
        assert!(store.sequence_at(&id, wrong, false).is_err());
    }
    assert_eq!(
        store.source_at("Types.java", Some(pin)).unwrap().unwrap().0,
        pin
    );
    assert_eq!(store.source_at("Types.java", None).unwrap().unwrap().0, pin);
    assert_eq!(store.symbol_at(&id, Some(pin)).unwrap().unwrap().0, pin);
    assert!(store.symbol(&id).unwrap().is_some());
    assert_eq!(
        store.files_at(Some(pin), 0, 10).unwrap()["revision"],
        json!(pin)
    );
    assert_eq!(
        store.methods_at("Types.java", Some(pin)).unwrap().unwrap()["revision"],
        json!(pin)
    );
    assert_eq!(
        store
            .classes_at(None, "", Some(pin), 0, 10)
            .unwrap()
            .revision,
        pin
    );
    assert_eq!(
        store
            .sequence_at(&id, pin, false)
            .unwrap()
            .unwrap()
            .revision,
        pin
    );
}
#[test]
fn current_store_producer_matrix() {
    let (temp, store, graph, id) = fixture();
    let pin = publish(&store, &graph, store.status().unwrap().revision);
    assert_eq!(store.symbols_at("run", 10).unwrap().0, pin);
    let query: ViewQuery = serde_json::from_value(json!({"seed":id})).unwrap();
    assert_eq!(store.query_view(&query).unwrap().unwrap().revision, pin);
    let mut items = baleyg::file_tree::SourceDir::open(&temp.path().join("workspace"))
        .unwrap()
        .list("", 0, 10)
        .unwrap()
        .0;
    assert_eq!(
        store
            .tree_metadata(&temp.path().join("workspace"), &mut items)
            .unwrap()
            .0,
        pin
    );
}
#[test]
fn pair_recreation_cas() {
    let (temp, store, graph, _id) = fixture();
    let first = store.status().unwrap().revision;
    let old = publish(&store, &graph, first);
    let index_root = temp.path().join("state/cache/indexes");
    let dir = std::fs::read_dir(&index_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|p| p.is_dir())
        .unwrap();
    let use_lock = index_root.join(format!(
        "{}.lock",
        dir.file_name().unwrap().to_string_lossy()
    ));
    drop(store);
    for child in ["index.db", "leader.lock"] {
        std::fs::remove_file(dir.join(child)).unwrap();
    }
    std::fs::remove_dir(&dir).unwrap();
    std::fs::remove_file(use_lock).unwrap();
    let root = temp.path().join("workspace");
    let recreated = crate::common::open_store(&temp.path().join("state"), &root).unwrap();
    let fresh = recreated.status().unwrap().revision;
    assert_eq!(fresh.index_revision, 0);
    assert_ne!(fresh.index_generation, old.index_generation);
    assert!(
        recreated
            .publish(
                &graph,
                &recreated.leader().unwrap(),
                first,
                &Arc::new(AtomicBool::new(false))
            )
            .is_err()
    );
    assert_eq!(publish(&recreated, &graph, fresh).index_revision, 1);
    assert!(recreated.source_at("Types.java", Some(old)).is_err());
}
