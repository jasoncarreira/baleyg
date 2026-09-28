mod common;
use baleyg::{model::*, store::Store};
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::AtomicBool},
};
use tempfile::TempDir;
fn private_state() -> TempDir {
    let state = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    state
}
fn fixture() -> (TempDir, TempDir, Store) {
    let state = private_state();
    let work = tempfile::tempdir().unwrap();
    let store = crate::common::open_store(state.path(), work.path()).unwrap();
    (state, work, store)
}
fn pin(store: &Store, revision: u64) -> IndexPin {
    IndexPin {
        index_generation: store.status().unwrap().revision.index_generation,
        index_revision: revision,
    }
}
// Model the old graph-only cache shape, rather than merely lowering its version.
fn downgrade_to_legacy(db: &rusqlite::Connection) {
    db.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    let tables = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name GLOB 'native_*'")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for table in tables {
        db.execute_batch(&format!("DROP TABLE {table}")).unwrap();
    }
    for index in ["nodes_path", "calls_path", "regions_path"] {
        db.execute_batch(&format!("DROP INDEX {index}")).unwrap();
    }
    db.execute(
        "UPDATE index_metadata SET schema_version=4,extractor_version='native-v1'",
        [],
    )
    .unwrap();
    db.pragma_update(None, "user_version", 4).unwrap();
}
fn index_db(state: &std::path::Path) -> std::path::PathBuf {
    std::fs::read_dir(state.join("cache/indexes"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
        .join("index.db")
}
fn cancel() -> CancelFlag {
    Arc::new(AtomicBool::new(false))
}
fn bundle(
    store: &Store,
    work: &TempDir,
) -> (
    Graph,
    baleyg::native_evidence::Artifact,
    baleyg::capture::Capture,
) {
    baleyg::indexer::index_workspace_bundle(
        &baleyg::indexer::IndexOptions::new(work.path().to_owned()),
        store.root_id(),
        &cancel(),
        |_| {},
    )
    .unwrap()
}
fn publish_bundle(
    store: &Store,
    bundle: &(
        Graph,
        baleyg::native_evidence::Artifact,
        baleyg::capture::Capture,
    ),
    expected: IndexPin,
) -> IndexPin {
    store
        .publish_native(
            &bundle.0,
            &bundle.2,
            &bundle.1,
            &store.leader().unwrap(),
            expected,
            &cancel(),
        )
        .unwrap()
}
fn write_source(work: &TempDir) {
    std::fs::write(
        work.path().join("a.js"),
        "function a() { b(); c(); }
function b() { c(); }
function c() { a(); }
",
    )
    .unwrap();
}
fn symbol_id(graph: &Graph, name: &str) -> String {
    graph
        .nodes
        .iter()
        .find(|node| node.name == name)
        .unwrap()
        .id
        .clone()
}
fn node(id: &str) -> Symbol {
    Symbol {
        id: id.into(),
        name: id.into(),
        kind: SymbolKind::Function,
        path: "a.js".into(),
        range: SourceRange {
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 1,
            ..SourceRange::default()
        },
        parent: None,
        accessor: false,
        provenance: Provenance {
            source: "syntax".into(),
            semantic: SemanticState::Unavailable,
        },
        display_label: None,
    }
}
fn call(id: &str, from: &str, to: &str) -> CallSite {
    CallSite {
        id: id.into(),
        caller: from.into(),
        callee_text: Some(to.into()),
        path: "a.js".into(),
        range: SourceRange {
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 1,
            ..SourceRange::default()
        },
        callee_range: None,
        ordinal: 0,
        regions: vec![],
        provenance: node("").provenance,
    }
}
fn query() -> ViewQuery {
    serde_json::from_str(r#"{"seed":"a"}"#).unwrap()
}
#[test]
fn publication_is_atomic_and_reopens() {
    let (state, work, store) = fixture();
    write_source(&work);
    let captured = bundle(&store, &work);
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    let baseline = store.index_baseline().unwrap();
    let before = std::fs::read(index_db(state.path())).unwrap();
    let leader = store.leader().unwrap();
    for error in [
        store
            .publish(&captured.0, &leader, baseline, &cancel())
            .unwrap_err(),
        store
            .publish_captured(&captured.0, &captured.2, &leader, baseline, &cancel())
            .unwrap_err(),
    ] {
        assert!(
            error.to_string().contains("native_evidence_required"),
            "{error:#}"
        );
    }
    drop(leader);
    assert_eq!(std::fs::read(index_db(state.path())).unwrap(), before);
    assert_eq!(store.index_baseline().unwrap(), baseline);
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    let first = publish_bundle(&store, &captured, baseline);
    assert_eq!(first.index_revision, 1);
    let old = store.graph().unwrap();
    let a = symbol_id(&old, "a");
    let mut duplicate = old.clone();
    duplicate.nodes.push(old.nodes[0].clone());
    assert!(
        store
            .publish_native(
                &duplicate,
                &captured.2,
                &captured.1,
                &store.leader().unwrap(),
                first,
                &cancel()
            )
            .is_err()
    );
    assert_eq!(store.graph().unwrap(), old);
    assert!(
        store
            .publish_native(
                &captured.0,
                &captured.2,
                &captured.1,
                &store.leader().unwrap(),
                baseline,
                &cancel()
            )
            .unwrap_err()
            .to_string()
            .starts_with("revision conflict")
    );
    assert!(
        store
            .publish_native(
                &captured.0,
                &captured.2,
                &captured.1,
                &store.leader().unwrap(),
                first,
                &Arc::new(AtomicBool::new(true))
            )
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    assert_eq!(store.status().unwrap().revision, first);
    drop(store);
    let store = Store::open_for_tests(state.path(), work.path()).unwrap();
    assert_eq!(store.graph().unwrap(), old);
    assert_eq!(
        store.source("a.js").unwrap().unwrap().text,
        captured.0.files[0].text
    );
    assert!(
        store
            .source_at("a.js", Some(pin(&store, 0)))
            .unwrap_err()
            .to_string()
            .starts_with("revision conflict")
    );
    assert_eq!(store.symbol_at(&a, Some(first)).unwrap().unwrap().0, first);
    assert_eq!(store.symbols_at("", 5).unwrap().0, first);
    let mut reverse = captured.0.clone();
    reverse.nodes.reverse();
    reverse.calls.reverse();
    let second = store
        .publish_native(
            &reverse,
            &captured.2,
            &captured.1,
            &store.leader().unwrap(),
            first,
            &cancel(),
        )
        .unwrap();
    assert_eq!(second.index_revision, 2);
    assert_eq!(store.graph().unwrap(), old);
}

#[test]
fn delete_reader_pins_snapshot_and_blocks_publish() {
    let (state, work, store) = fixture();
    write_source(&work);
    let captured = bundle(&store, &work);
    let first = publish_bundle(&store, &captured, store.index_baseline().unwrap());
    let leader = store.leader().unwrap();
    let db = rusqlite::Connection::open(index_db(state.path())).unwrap();
    db.busy_timeout(std::time::Duration::ZERO).unwrap();
    db.execute_batch("BEGIN").unwrap();
    let revision = || {
        db.query_row("SELECT index_revision FROM index_metadata", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap()
    };
    assert_eq!(revision(), 1);
    assert!(
        store
            .publish_native(
                &captured.0,
                &captured.2,
                &captured.1,
                &leader,
                first,
                &cancel()
            )
            .is_err()
    );
    assert_eq!(revision(), 1);
    assert_eq!(
        db.query_row("SELECT count(*) FROM nodes", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        captured.0.nodes.len() as i64
    );
    db.execute_batch("COMMIT").unwrap();
    let next = store
        .publish_native(
            &captured.0,
            &captured.2,
            &captured.1,
            &leader,
            first,
            &cancel(),
        )
        .unwrap();
    assert_eq!(revision(), 2);
    assert_eq!(next.index_revision, 2);
}

#[test]
fn incompatible_index_refuses_without_touching_legacy_state() {
    let (state, work, store) = fixture();
    let index = index_db(state.path());
    drop(store);
    let legacy = state.path().join("cache.db");
    std::fs::write(&legacy, "untouched legacy bytes").unwrap();
    let db = rusqlite::Connection::open(&index).unwrap();
    db.pragma_update(None, "user_version", 99).unwrap();
    drop(db);
    assert!(crate::common::open_store(state.path(), work.path()).is_err());
    assert_eq!(std::fs::read(legacy).unwrap(), b"untouched legacy bytes");
}
#[test]
fn literal_search_and_snapshot_only_sources() {
    let (_state, work, store) = fixture();
    // Odd names are not valid JavaScript declarations. Search metacharacters must
    // still be literal rather than SQL wildcards against real indexed names.
    std::fs::write(
        work.path().join("a.js"),
        "function abc() {}
function a_c() {}
function percent() {}
",
    )
    .unwrap();
    let captured = bundle(&store, &work);
    publish_bundle(&store, &captured, store.index_baseline().unwrap());
    assert_eq!(store.symbols("a_", 1000).unwrap().len(), 1);
    assert!(store.symbols("%_", 1000).unwrap().is_empty());
    assert!(store.symbols("' OR 1=1 --no", 1000).unwrap().is_empty());
    assert!(store.symbol("' OR 1=1 --").unwrap().is_none());
    std::fs::write(work.path().join("a.js"), "changed live source").unwrap();
    assert_eq!(
        store.source("a.js").unwrap().unwrap().text,
        captured.0.files[0].text
    );
    assert!(store.source("../a.js").unwrap().is_none());
}

#[test]
fn traversal_cycles_bounds_callbacks_and_boundaries() {
    let (_state, work, store) = fixture();
    write_source(&work);
    let captured = bundle(&store, &work);
    publish_bundle(&store, &captured, store.index_baseline().unwrap());
    let mut q = query();
    q.seed = symbol_id(&captured.0, "a");
    let seed = store.query_view(&q).unwrap().unwrap();
    assert_eq!(seed.nodes.len(), 1);
    assert_eq!(seed.nodes[0].id, q.seed);
    assert!(!seed.calls.is_empty());
    q.include_callbacks = true;
    q.depth = 5;
    let inert = store.query_view(&q).unwrap().unwrap();
    assert_eq!(inert.nodes, seed.nodes);
    assert_eq!(inert.calls, seed.calls);
    q.max_nodes = 1;
    assert!(!store.query_view(&q).unwrap().unwrap().truncated);
    q.max_calls = 1;
    let bounded = store.query_view(&q).unwrap().unwrap();
    assert_eq!(bounded.calls.len(), 1);
    assert!(bounded.truncated);
    q.max_calls = 0;
    assert!(store.query_view(&q).is_err());
    q.max_calls = 1;
    q.exclude_paths.push("a.js".into());
    let excluded = store.query_view(&q).unwrap().unwrap();
    assert_eq!(excluded.nodes.len(), 1);
    assert!(excluded.calls.is_empty());
    q.seed = "missing".into();
    assert!(store.query_view(&q).unwrap().is_none());
    q.depth = 6;
    assert!(store.query_view(&q).is_err());
}

#[test]
fn durable_user_data_survives_cache_loss_and_resolves_orphans() {
    let (state, work, store) = fixture();
    write_source(&work);
    let captured = bundle(&store, &work);
    publish_bundle(&store, &captured, store.index_baseline().unwrap());
    let a = symbol_id(&captured.0, "a");
    let b = symbol_id(&captured.0, "b");
    let annotation = Annotation {
        id: "note".into(),
        node_id: a.clone(),
        body: "hello".into(),
    };
    store.put_annotation(&annotation).unwrap();
    let mut view_query = query();
    view_query.seed = a.clone();
    let view = SavedView {
        id: "view".into(),
        title: "Title".into(),
        query: view_query,
        pins: BTreeMap::from([("missing".into(), Position { x: 1., y: 2. })]),
        hidden: vec![b.clone()],
    };
    store.put_view(&view).unwrap();
    let saved_note = store.annotations().unwrap().remove(0);
    assert!(saved_note.orphaned);
    assert_eq!(
        saved_note.attachment.availability,
        AttachmentAvailability::Anchorless
    );
    let saved_view = store.views().unwrap().remove(0);
    assert!(saved_view.orphaned_ids.contains(&a));
    assert!(saved_view.orphaned_ids.contains(&"missing".into()));
    assert_eq!(
        saved_view.attachment.availability,
        AttachmentAvailability::Anchorless
    );
    drop(store);
    std::fs::remove_file(index_db(state.path())).unwrap();
    let store = crate::common::open_store(state.path(), work.path()).unwrap();
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    assert_eq!(store.index_baseline().unwrap().index_revision, 0);
    let unavailable_note = store.annotations().unwrap().remove(0);
    assert!(unavailable_note.orphaned);
    assert_eq!(
        unavailable_note.attachment.availability,
        AttachmentAvailability::IndexUnavailable
    );
    assert_eq!(unavailable_note.annotation, annotation);
    let orphaned = store.view("view").unwrap().unwrap().orphaned_ids;
    assert_eq!(orphaned.len(), 3);
    assert!(orphaned.contains(&a) && orphaned.contains(&b) && orphaned.contains(&"missing".into()));
    let fresh = bundle(&store, &work);
    publish_bundle(&store, &fresh, store.index_baseline().unwrap());
    let restored_note = store.annotations().unwrap().remove(0);
    assert!(restored_note.orphaned);
    assert_eq!(
        restored_note.attachment.availability,
        AttachmentAvailability::Anchorless
    );
    // Public graph-only writes cannot remove indexed symbols.
    assert!(
        store
            .publish(
                &Graph::default(),
                &store.leader().unwrap(),
                store.status().unwrap().revision,
                &cancel()
            )
            .unwrap_err()
            .to_string()
            .contains("native_evidence_required")
    );
    std::fs::write(
        work.path().join("a.js"),
        "function c() {}
",
    )
    .unwrap();
    let removed = bundle(&store, &work);
    publish_bundle(&store, &removed, store.status().unwrap().revision);
    assert!(store.annotations().unwrap()[0].orphaned);
    assert!(store.delete_annotation("note").unwrap());
    assert!(!store.delete_annotation("note").unwrap());
    assert!(store.delete_view("view").unwrap());
    assert!(store.view("view").unwrap().is_none());
    let mut invalid = view;
    invalid
        .pins
        .insert(a.clone(), Position { x: f64::NAN, y: 0. });
    assert!(store.put_view(&invalid).is_err());
    assert!(
        store
            .put_annotation(&Annotation {
                id: "".into(),
                node_id: a,
                body: "".into()
            })
            .is_err()
    );
}

#[test]
fn concurrent_publish_cas_has_one_winner() {
    let (_state, work, store) = fixture();
    write_source(&work);
    let captured = Arc::new(bundle(&store, &work));
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let baseline = store.index_baseline().unwrap();
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            let captured = captured.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let leader = store.leader()?;
                store.publish_native(
                    &captured.0,
                    &captured.2,
                    &captured.1,
                    &leader,
                    baseline,
                    &cancel(),
                )
            })
        })
        .collect();
    barrier.wait();
    let outcomes: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(outcomes.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(
        outcomes
            .iter()
            .find_map(|r| r.as_ref().err())
            .unwrap()
            .to_string()
            .starts_with("revision conflict")
            || outcomes.iter().any(|r| r
                .as_ref()
                .err()
                .is_some_and(|e| e.to_string().starts_with("storage_busy")))
    );
    assert_eq!(store.status().unwrap().revision.index_revision, 1);
}

#[test]
fn malformed_graph_rolls_back_and_structural_stats_are_recounted() {
    let (_state, work, store) = fixture();
    write_source(&work);
    let captured = bundle(&store, &work);
    let first = publish_bundle(&store, &captured, store.index_baseline().unwrap());
    let baseline = store.graph().unwrap();
    assert_eq!(baseline.stats.symbols, captured.0.nodes.len());
    assert_eq!(baseline.stats.internal, 0);
    assert_eq!(baseline.stats.unresolved, captured.0.calls.len());
    assert!(!baseline.calls.is_empty());
    for kind in 0..7 {
        let mut bad = captured.0.clone();
        match kind {
            0 => bad.calls[0].caller = "missing".into(),
            1 => bad.calls[0].id = bad.calls[1].id.clone(),
            2 => bad.calls[0].regions.push("missing".into()),
            3 => bad.calls[0].range.end_byte = usize::MAX,
            4 => bad.nodes[0].parent = Some("missing".into()),
            5 => bad.nodes[0].range.end_byte = usize::MAX,
            _ => bad.nodes[0].range.start_line = 0,
        }
        assert!(
            store
                .publish_native(
                    &bad,
                    &captured.2,
                    &captured.1,
                    &store.leader().unwrap(),
                    first,
                    &cancel()
                )
                .is_err(),
            "case {kind}"
        );
        assert_eq!(store.status().unwrap().revision, first);
        assert_eq!(store.graph().unwrap(), baseline);
    }
}

#[test]
fn cache_loss_never_reuses_revision_tokens_and_sql_enforces_foreign_keys() {
    let (state, work, store) = fixture();
    write_source(&work);
    let captured = bundle(&store, &work);
    let rev = publish_bundle(&store, &captured, store.index_baseline().unwrap());
    let db = rusqlite::Connection::open(index_db(state.path())).unwrap();
    db.pragma_update(None, "foreign_keys", true).unwrap();
    assert!(
        db.execute("DELETE FROM files WHERE path='a.js'", [])
            .is_err()
    );
    let a = symbol_id(&captured.0, "a");
    assert!(db.execute("DELETE FROM nodes WHERE id=?1", [&a]).is_err());
    drop(db);
    drop(store);
    std::fs::remove_file(index_db(state.path())).unwrap();
    let store = crate::common::open_store(state.path(), work.path()).unwrap();
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    let fresh = bundle(&store, &work);
    assert!(
        store
            .publish_native(
                &fresh.0,
                &fresh.2,
                &fresh.1,
                &store.leader().unwrap(),
                rev,
                &cancel()
            )
            .is_err()
    );
    let new_rev = publish_bundle(&store, &fresh, store.index_baseline().unwrap());
    assert_ne!(new_rev.index_generation, rev.index_generation);
    assert!(store.source_at("a.js", Some(rev)).is_err());
}

#[test]
fn refuses_unversioned_existing_index_without_migration() {
    let (state, work, store) = fixture();
    let index = index_db(state.path());
    drop(store);
    let db = rusqlite::Connection::open(&index).unwrap();
    db.pragma_update(None, "user_version", 1).unwrap();
    drop(db);
    assert!(crate::common::open_store(state.path(), work.path()).is_err());
    assert!(index.exists());
}

#[test]
fn graph_call_dto_rejects_lexical_proof_fields() {
    let call = call("ab", "a", "b");
    let mut value = serde_json::to_value(call).unwrap();
    for field in [
        "target",
        "candidateSymbols",
        "resolution",
        "callbackArguments",
    ] {
        value[field] = serde_json::json!("unsupported");
        assert!(
            serde_json::from_value::<CallSite>(value.clone()).is_err(),
            "{field}"
        );
        value.as_object_mut().unwrap().remove(field);
    }
}

#[test]
fn active_delete_journal_allows_prior_pair_or_busy_and_cold_journal_is_sqlite_managed() {
    let (state, work, store) = fixture();
    write_source(&work);
    let captured = bundle(&store, &work);
    let previous = publish_bundle(&store, &captured, store.index_baseline().unwrap());
    let leader = store.leader().unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let path = index_db(state.path());
    let writer = std::thread::spawn(move || {
        let db = rusqlite::Connection::open(path).unwrap();
        db.execute_batch(
            "BEGIN IMMEDIATE; UPDATE index_metadata SET index_revision=2 WHERE singleton=1",
        )
        .unwrap();
        ready_tx.send(()).unwrap();
        done_rx.recv().unwrap();
        db.execute_batch("ROLLBACK").unwrap();
        drop(leader);
    });
    ready_rx.recv().unwrap();
    assert!(
        index_db(state.path())
            .with_file_name("index.db-journal")
            .exists()
    );
    match store.status() {
        Ok(status) => assert_eq!(status.revision, previous),
        Err(e) => assert!(e.to_string().contains("storage_busy"), "{e:#}"),
    }
    match store.source_at("a.js", Some(previous)) {
        Ok(Some((pin, source))) => {
            assert_eq!(pin, previous);
            assert_eq!(source.text, captured.0.files[0].text);
        }
        Err(e) => assert!(e.to_string().contains("storage_busy"), "{e:#}"),
        Ok(other) => panic!("unexpected pinned source: {other:?}"),
    }
    done_tx.send(()).unwrap();
    writer.join().unwrap();
    let journal = index_db(state.path()).with_file_name("index.db-journal");
    std::fs::write(&journal, [0u8; 512]).unwrap();
    assert_eq!(store.status().unwrap().revision, previous);
    assert!(
        store
            .leader()
            .unwrap_err()
            .to_string()
            .contains("recovery_required")
    );
    assert!(journal.exists());
}

#[test]
fn legacy_cache_is_control_only_until_lock_safe_rebaseline_rotates_full_pair() {
    let (state, work, store) = fixture();
    std::fs::write(
        work.path().join("a.js"),
        "function one() { console.log('measured'); }",
    )
    .unwrap();
    let (graph, native, capture) = bundle(&store, &work);
    let first = store
        .publish_native(
            &graph,
            &capture,
            &native,
            &store.leader().unwrap(),
            store.index_baseline().unwrap(),
            &cancel(),
        )
        .unwrap();
    let path = index_db(state.path());
    drop(store);
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        downgrade_to_legacy(&db);
        // This old cached row contains a lexical proof field that MUST never escape.
        db.execute("UPDATE calls SET payload=json_set(payload, '$.target', 'guessed-node', '$.resolution','internal')", []).unwrap();
    }
    let store = Store::open_for_tests(state.path(), work.path()).unwrap();
    assert_eq!(store.index_baseline().unwrap(), first);
    for result in [
        store.status().map(|_| ()),
        store.graph().map(|_| ()),
        store.symbols_at("", 10).map(|_| ()),
        store.source("a.js").map(|_| ()),
        store
            .query_view(&ViewQuery {
                seed: graph
                    .nodes
                    .iter()
                    .find(|n| n.name == "one")
                    .unwrap()
                    .id
                    .clone(),
                depth: 1,
                max_nodes: 40,
                max_calls: 200,
                include_callbacks: true,
                exclude_paths: vec![],
            })
            .map(|_| ()),
    ] {
        assert!(result.unwrap_err().to_string().contains("index_not_ready"));
    }
    let old_bytes = std::fs::read(&path).unwrap();
    let rejected = store.publish_native(
        &graph,
        &capture,
        &native,
        &store.leader().unwrap(),
        first,
        &Arc::new(AtomicBool::new(true)),
    );
    assert!(rejected.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), old_bytes);
    assert!(store.status().is_err());
    let assert_old = || {
        assert_eq!(std::fs::read(&path).unwrap(), old_bytes);
        assert_eq!(store.index_baseline().unwrap(), first);
        assert!(
            store
                .status()
                .unwrap_err()
                .to_string()
                .contains("index_not_ready")
        );
        assert!(
            store
                .graph()
                .unwrap_err()
                .to_string()
                .contains("index_not_ready")
        );
        let db = rusqlite::Connection::open(&path).unwrap();
        let (version, marker): (i64, String) = db
            .query_row(
                "SELECT schema_version,extractor_version FROM index_metadata",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((version, marker.as_str()), (4, "native-v1"));
        assert_eq!(
            db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            4
        );
    };
    let mut stale = first;
    stale.index_revision += 1;
    assert!(
        store
            .publish_native(
                &graph,
                &capture,
                &native,
                &store.leader().unwrap(),
                stale,
                &cancel()
            )
            .unwrap_err()
            .to_string()
            .starts_with("revision conflict")
    );
    assert_old();
    let (_other_state, _other_work, other_store) = fixture();
    assert!(
        store
            .publish_native(
                &graph,
                &capture,
                &native,
                &other_store.leader().unwrap(),
                first,
                &cancel()
            )
            .unwrap_err()
            .to_string()
            .contains("leader")
    );
    assert_old();
    // The captured source, root, use and leader locks, and full expected pair all
    // participate in the same SQLite transaction before the new marker appears.
    let next = store
        .publish_native(
            &graph,
            &capture,
            &native,
            &store.leader().unwrap(),
            first,
            &cancel(),
        )
        .unwrap();
    assert_ne!(next.index_generation, first.index_generation);
    assert_eq!(next.index_revision, first.index_revision + 1);
    assert_eq!(
        store.status().unwrap().evidence_format.as_deref(),
        Some("terminal-native-graph-v1")
    );
    assert!(
        store
            .source_at("a.js", Some(first))
            .unwrap_err()
            .to_string()
            .contains("revision conflict")
    );
    let saved = store.graph().unwrap();
    assert_eq!(saved.calls, graph.calls);
    assert!(
        !serde_json::to_string(&saved)
            .unwrap()
            .contains("guessed-node")
    );
    assert!(
        store
            .publish_native(
                &graph,
                &capture,
                &native,
                &store.leader().unwrap(),
                first,
                &cancel()
            )
            .unwrap_err()
            .to_string()
            .starts_with("revision conflict")
    );
}

#[test]
fn legacy_unknown_trigger_is_refused_before_any_rebaseline_write_or_forged_export() {
    let (state, work, store) = fixture();
    std::fs::write(work.path().join("a.js"), "function foo() { bar(); }\n").unwrap();
    let (graph, native, capture) = bundle(&store, &work);
    let first = store
        .publish_native(
            &graph,
            &capture,
            &native,
            &store.leader().unwrap(),
            store.index_baseline().unwrap(),
            &cancel(),
        )
        .unwrap();
    let path = index_db(state.path());
    let leader = store.leader().unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    downgrade_to_legacy(&db);
    db.execute_batch(
        "CREATE TRIGGER forged_call AFTER INSERT ON calls BEGIN
        UPDATE calls SET payload=json_set(payload,'$.calleeText','FORGED-NOT-MEASURED')
        WHERE id=NEW.id; END;",
    )
    .unwrap();
    drop(db);
    let old_bytes = std::fs::read(&path).unwrap();
    assert!(
        Store::open_for_tests(state.path(), work.path())
            .unwrap_err()
            .to_string()
            .contains("incompatible_index: unknown cache object")
    );
    let error = store
        .publish_native(&graph, &capture, &native, &leader, first, &cancel())
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("incompatible_index: unknown cache object"),
        "{error:#}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), old_bytes);
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );
    assert!(
        store
            .graph()
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );
    let db = rusqlite::Connection::open(&path).unwrap();
    let (version, generation, revision): (i64, String, i64) = db
        .query_row(
            "SELECT schema_version,index_generation,index_revision FROM index_metadata",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (version, generation, revision),
        (
            4,
            first.index_generation.to_string(),
            first.index_revision as i64
        )
    );
    assert_eq!(
        db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        4
    );
    let forged: i64 = db
        .query_row(
            "SELECT count(*) FROM calls WHERE payload LIKE '%FORGED-NOT-MEASURED%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        forged, 0,
        "unknown trigger executed during rejected rebaseline"
    );
}

#[test]
fn safe_cache_unknown_view_blocks_public_status_and_source_until_owner_intervenes() {
    let (state, work, store) = fixture();
    write_source(&work);
    let captured = bundle(&store, &work);
    let before = publish_bundle(&store, &captured, store.index_baseline().unwrap());
    let leader = store.leader().unwrap();
    let path = index_db(state.path());
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE VIEW unapproved_view AS SELECT 1")
        .unwrap();
    drop(db);
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        Store::open_for_tests(state.path(), work.path())
            .unwrap_err()
            .to_string()
            .contains("incompatible_index: unknown cache object")
    );
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );
    assert!(
        store
            .graph()
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );
    assert!(
        store
            .publish_native(
                &captured.0,
                &captured.2,
                &captured.1,
                &leader,
                before,
                &cancel()
            )
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("DROP VIEW unapproved_view").unwrap();
    assert_eq!(store.status().unwrap().revision, before);
}

#[test]
fn legacy_rebaseline_changed_root_refuses_without_rewriting_known_old_bytes() {
    let (state, work, store) = fixture();
    std::fs::write(work.path().join("a.js"), "function one() { foo(); }\n").unwrap();
    let (graph, native, capture) = bundle(&store, &work);
    let old = store
        .publish_native(
            &graph,
            &capture,
            &native,
            &store.leader().unwrap(),
            store.index_baseline().unwrap(),
            &cancel(),
        )
        .unwrap();
    let path = index_db(state.path());
    let db = rusqlite::Connection::open(&path).unwrap();
    downgrade_to_legacy(&db);
    drop(db);
    let old_bytes = std::fs::read(&path).unwrap();
    let leader = store.leader().unwrap();
    let moved = work.path().with_extension("temporarily-moved");
    std::fs::rename(work.path(), &moved).unwrap();
    let refused = store
        .publish_native(&graph, &capture, &native, &leader, old, &cancel())
        .unwrap_err();
    assert!(
        refused.to_string().contains("root_changed") || refused.to_string().contains("root drift")
    );
    assert_eq!(std::fs::read(&path).unwrap(), old_bytes);
    std::fs::rename(moved, work.path()).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    let pair: (i64, String, String, i64) = db.query_row(
        "SELECT schema_version,extractor_version,index_generation,index_revision FROM index_metadata", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
    assert_eq!(
        pair,
        (
            4,
            "native-v1".into(),
            old.index_generation.to_string(),
            old.index_revision as i64
        )
    );
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
}

#[test]
fn legacy_rebaseline_capture_drift_after_partial_write_preserves_old_bytes() {
    let (state, work, store) = fixture();
    let source = work.path().join("a.js");
    std::fs::write(&source, "function one() { foo(); }\n").unwrap();
    let (graph, native, capture) = bundle(&store, &work);
    let old = store
        .publish_native(
            &graph,
            &capture,
            &native,
            &store.leader().unwrap(),
            store.index_baseline().unwrap(),
            &cancel(),
        )
        .unwrap();
    let path = index_db(state.path());
    let db = rusqlite::Connection::open(&path).unwrap();
    downgrade_to_legacy(&db);
    drop(db);
    let old_bytes = std::fs::read(&path).unwrap();
    let leader = store.leader().unwrap();
    // Capture verifies again after all projection INSERTs and metadata UPDATE.
    std::fs::write(&source, "function changed() { notInCapture(); }\n").unwrap();
    let refused = store
        .publish_native(&graph, &capture, &native, &leader, old, &cancel())
        .unwrap_err();
    assert!(refused.to_string().contains("drift"), "{refused:#}");
    assert_eq!(std::fs::read(&path).unwrap(), old_bytes);
    let db = rusqlite::Connection::open(&path).unwrap();
    let pair: (i64, String, String, i64) = db.query_row(
        "SELECT schema_version,extractor_version,index_generation,index_revision FROM index_metadata", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
    assert_eq!(
        pair,
        (
            4,
            "native-v1".into(),
            old.index_generation.to_string(),
            old.index_revision as i64
        )
    );
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
}

#[test]
fn saved_reads_without_records_are_conservative_and_write_nothing() {
    let (state, work, store) = fixture();
    let cache = index_db(state.path());
    let before = std::fs::read(&cache).unwrap();
    assert!(store.views().unwrap().is_empty());
    assert!(store.annotations().unwrap().is_empty());
    assert!(store.view("missing").unwrap().is_none());
    assert_eq!(
        std::fs::read(&cache).unwrap(),
        before,
        "saved reads changed the cache database"
    );
    let identity =
        baleyg::store::topology::WorkspaceIdentity::discover(Some(work.path()), work.path())
            .unwrap();
    let roots = baleyg::store::topology::TopologyRoots::isolated_for_tests(
        state.path().join("cache"),
        state.path().join("data"),
    );
    assert!(
        !roots.record_db(&identity).exists(),
        "saved reads created a durable database"
    );
}
