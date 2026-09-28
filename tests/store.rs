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
fn graph() -> Graph {
    Graph {
        files: vec![SourceFile {
            path: "a.js".into(),
            hash: "hash".into(),
            language: "javascript".into(),
            text: "function a() {}".into(),
        }],
        nodes: vec![node("a"), node("b"), node("c")],
        calls: vec![
            call("ab", "a", "b"),
            call("bc", "b", "c"),
            call("ca", "c", "a"),
        ],
        ..Graph::default()
    }
}
fn query() -> ViewQuery {
    serde_json::from_str(r#"{"seed":"a"}"#).unwrap()
}
#[test]
fn publication_is_atomic_and_reopens() {
    let (state, work, store) = fixture();
    let graph = graph();
    assert_eq!(store.status().unwrap().revision.index_revision, 0);
    assert_eq!(
        store
            .publish(
                &graph,
                &store.leader().unwrap(),
                baleyg::model::IndexPin {
                    index_generation: store.status().unwrap().revision.index_generation,
                    index_revision: 0
                },
                &cancel()
            )
            .unwrap()
            .index_revision,
        1
    );
    let old = store.graph().unwrap();
    let mut duplicate = graph.clone();
    duplicate.nodes.push(node("a"));
    assert!(
        store
            .publish(
                &duplicate,
                &store.leader().unwrap(),
                baleyg::model::IndexPin {
                    index_generation: store.status().unwrap().revision.index_generation,
                    index_revision: 1
                },
                &cancel()
            )
            .is_err()
    );
    assert_eq!(store.graph().unwrap(), old);
    assert!(
        store
            .publish(
                &Graph::default(),
                &store.leader().unwrap(),
                baleyg::model::IndexPin {
                    index_generation: store.status().unwrap().revision.index_generation,
                    index_revision: 0
                },
                &cancel()
            )
            .unwrap_err()
            .to_string()
            .starts_with("revision conflict")
    );
    assert!(
        store
            .publish(
                &Graph::default(),
                &store.leader().unwrap(),
                store.status().unwrap().revision,
                &Arc::new(AtomicBool::new(true))
            )
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    assert_eq!(store.status().unwrap().revision.index_revision, 1);
    drop(store);
    let store = crate::common::open_store(state.path(), work.path()).unwrap();
    assert_eq!(store.graph().unwrap(), old);
    assert_eq!(
        store.source("a.js").unwrap().unwrap().text,
        "function a() {}"
    );
    assert!(
        store
            .source_at("a.js", Some(pin(&store, 0)))
            .unwrap_err()
            .to_string()
            .starts_with("revision conflict")
    );
    assert_eq!(
        store
            .symbol_at("a", Some(pin(&store, 1)))
            .unwrap()
            .unwrap()
            .0
            .index_revision,
        1
    );
    assert_eq!(store.symbols_at("", 5).unwrap().0.index_revision, 1);
    let mut reverse = graph.clone();
    reverse.nodes.reverse();
    reverse.calls.reverse();
    store
        .publish(
            &reverse,
            &store.leader().unwrap(),
            baleyg::model::IndexPin {
                index_generation: store.status().unwrap().revision.index_generation,
                index_revision: 1,
            },
            &cancel(),
        )
        .unwrap();
    assert_eq!(store.graph().unwrap(), old);
}
#[test]
fn delete_reader_pins_snapshot_and_blocks_publish() {
    let (state, _work, store) = fixture();
    let baseline = store.status().unwrap().revision;
    store
        .publish(&graph(), &store.leader().unwrap(), baseline, &cancel())
        .unwrap();
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
            .publish(
                &Graph::default(),
                &leader,
                store.status().unwrap().revision,
                &cancel()
            )
            .is_err()
    );
    assert_eq!(revision(), 1);
    assert_eq!(
        db.query_row("SELECT count(*) FROM nodes", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    db.execute_batch("COMMIT").unwrap();
    store
        .publish(
            &Graph::default(),
            &leader,
            store.status().unwrap().revision,
            &cancel(),
        )
        .unwrap();
    assert_eq!(revision(), 2);
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
    let mut graph = graph();
    graph.nodes.push(node("a%_' OR 1=1 --"));
    store
        .publish(
            &graph,
            &store.leader().unwrap(),
            store.status().unwrap().revision,
            &cancel(),
        )
        .unwrap();
    assert_eq!(store.symbols("%_", 1000).unwrap().len(), 1);
    assert!(store.symbols("' OR 1=1 --no", 1000).unwrap().is_empty());
    assert!(store.symbol("' OR 1=1 --").unwrap().is_none());
    std::fs::write(work.path().join("a.js"), "changed live source").unwrap();
    assert_eq!(
        store.source("a.js").unwrap().unwrap().text,
        "function a() {}"
    );
    assert!(store.source("../a.js").unwrap().is_none());
}
#[test]
fn traversal_cycles_bounds_callbacks_and_boundaries() {
    let (_state, _work, store) = fixture();
    let mut graph = graph();
    graph.nodes.push(node("callback"));
    graph.nodes.push(node("external"));
    graph.calls.push(call("ae", "a", "external"));
    store
        .publish(
            &graph,
            &store.leader().unwrap(),
            store.status().unwrap().revision,
            &cancel(),
        )
        .unwrap();
    let mut q = query();
    let seed = store.query_view(&q).unwrap().unwrap();
    assert_eq!(
        seed.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
        vec!["a"]
    );
    assert_eq!(seed.calls.len(), 2);
    q.include_callbacks = true;
    q.depth = 5;
    let inert = store.query_view(&q).unwrap().unwrap();
    assert_eq!(inert.nodes, seed.nodes);
    assert_eq!(inert.calls, seed.calls);
    assert!(
        !inert
            .nodes
            .iter()
            .any(|n| matches!(n.id.as_str(), "callback" | "external"))
    );
    q.max_nodes = 1;
    assert!(!store.query_view(&q).unwrap().unwrap().truncated);
    q.max_calls = 1;
    let bounded = store.query_view(&q).unwrap().unwrap();
    assert_eq!(bounded.calls.len(), 1);
    assert!(bounded.truncated);
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
    store
        .publish(
            &graph(),
            &store.leader().unwrap(),
            store.status().unwrap().revision,
            &cancel(),
        )
        .unwrap();
    let annotation = Annotation {
        id: "note".into(),
        node_id: "a".into(),
        body: "hello".into(),
    };
    store.put_annotation(&annotation).unwrap();
    let view = SavedView {
        id: "view".into(),
        title: "Title".into(),
        query: query(),
        pins: BTreeMap::from([("missing".into(), Position { x: 1., y: 2. })]),
        hidden: vec!["b".into()],
    };
    store.put_view(&view).unwrap();
    assert!(!store.annotations().unwrap()[0].orphaned);
    assert_eq!(store.views().unwrap()[0].orphaned_ids, vec!["missing"]);
    drop(store);
    std::fs::remove_file(index_db(state.path())).unwrap();
    let store = crate::common::open_store(state.path(), work.path()).unwrap();
    assert_eq!(store.status().unwrap().revision.index_revision, 0);
    assert!(store.annotations().unwrap()[0].orphaned);
    assert_eq!(store.annotations().unwrap()[0].annotation, annotation);
    assert_eq!(
        store.view("view").unwrap().unwrap().orphaned_ids,
        vec!["a", "b", "missing"]
    );
    store
        .publish(
            &graph(),
            &store.leader().unwrap(),
            store.status().unwrap().revision,
            &cancel(),
        )
        .unwrap();
    assert!(!store.annotations().unwrap()[0].orphaned);
    store
        .publish(
            &Graph::default(),
            &store.leader().unwrap(),
            store.status().unwrap().revision,
            &cancel(),
        )
        .unwrap();
    assert!(store.annotations().unwrap()[0].orphaned);
    assert!(store.delete_annotation("note").unwrap());
    assert!(!store.delete_annotation("note").unwrap());
    assert!(store.delete_view("view").unwrap());
    assert!(store.view("view").unwrap().is_none());
    let mut invalid = view;
    invalid
        .pins
        .insert("a".into(), Position { x: f64::NAN, y: 0. });
    assert!(store.put_view(&invalid).is_err());
    assert!(
        store
            .put_annotation(&Annotation {
                id: "".into(),
                node_id: "a".into(),
                body: "".into()
            })
            .is_err()
    );
}

#[test]
fn cancellation_during_transaction_rolls_back() {
    let (state, _work, store) = fixture();
    store
        .publish(
            &graph(),
            &store.leader().unwrap(),
            store.status().unwrap().revision,
            &cancel(),
        )
        .unwrap();
    let mut large = Graph {
        files: graph().files,
        ..Graph::default()
    };
    large.nodes = (0..30_000).map(|i| node(&format!("node-{i:05}"))).collect();
    let flag = cancel();
    let worker_flag = flag.clone();
    let worker_store = store.clone();
    let expected = worker_store.status().unwrap().revision;
    let leader = worker_store.leader().unwrap();
    let db = rusqlite::Connection::open(index_db(state.path())).unwrap();
    db.busy_timeout(std::time::Duration::ZERO).unwrap();
    let worker =
        std::thread::spawn(move || worker_store.publish(&large, &leader, expected, &worker_flag));
    // A competing BEGIN IMMEDIATE is the only observation available without a
    // production hook. Do not spin on it: a zero-timeout contender can otherwise
    // keep taking the writer slot before the publisher gets to its transaction.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match db.execute_batch("BEGIN IMMEDIATE") {
            Ok(()) => {
                db.execute_batch("ROLLBACK").unwrap();
            }
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::DatabaseBusy =>
            {
                break;
            }
            Err(error) => panic!("unexpected SQLite error: {error}"),
        }
        if worker.is_finished() {
            panic!(
                "publisher finished without an observed write transaction: {:?}",
                worker.join().unwrap()
            );
        }
        assert!(
            std::time::Instant::now() < deadline,
            "publisher never acquired write transaction"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    flag.store(true, std::sync::atomic::Ordering::Release);
    assert!(
        worker
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    assert_eq!(store.status().unwrap().revision.index_revision, 1);
    assert_eq!(store.graph().unwrap().nodes.len(), 3);
}

#[test]
fn concurrent_publish_cas_has_one_winner() {
    let (_state, _work, store) = fixture();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let baseline = store.status().unwrap().revision;
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let leader = store.leader()?;
                store.publish(&graph(), &leader, baseline, &cancel())
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
    let (_state, _work, store) = fixture();
    store
        .publish(
            &graph(),
            &store.leader().unwrap(),
            store.status().unwrap().revision,
            &cancel(),
        )
        .unwrap();
    let baseline = store.graph().unwrap();
    assert_eq!(baseline.stats.symbols, 3);
    assert_eq!(baseline.stats.internal, 0);
    assert_eq!(baseline.stats.unresolved, 3);
    for kind in 0..7 {
        let mut bad = graph();
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
                .publish(
                    &bad,
                    &store.leader().unwrap(),
                    baleyg::model::IndexPin {
                        index_generation: store.status().unwrap().revision.index_generation,
                        index_revision: 1
                    },
                    &cancel()
                )
                .is_err(),
            "case {kind}"
        );
        assert_eq!(store.status().unwrap().revision.index_revision, 1);
        assert_eq!(store.graph().unwrap(), baseline);
    }
}
#[test]
fn cache_loss_never_reuses_revision_tokens_and_sql_enforces_foreign_keys() {
    let (state, work, store) = fixture();
    let rev = store
        .publish(
            &graph(),
            &store.leader().unwrap(),
            baleyg::model::IndexPin {
                index_generation: store.status().unwrap().revision.index_generation,
                index_revision: 0,
            },
            &cancel(),
        )
        .unwrap();
    let db = rusqlite::Connection::open(index_db(state.path())).unwrap();
    db.pragma_update(None, "foreign_keys", true).unwrap();
    assert!(
        db.execute("DELETE FROM files WHERE path='a.js'", [])
            .is_err()
    );
    assert!(db.execute("DELETE FROM nodes WHERE id='a'", []).is_err());
    drop(db);
    drop(store);
    std::fs::remove_file(index_db(state.path())).unwrap();
    let store = crate::common::open_store(state.path(), work.path()).unwrap();
    assert_eq!(store.status().unwrap().revision.index_revision, 0);
    assert!(
        store
            .publish(&graph(), &store.leader().unwrap(), rev, &cancel())
            .is_err()
    );
    let new_rev = store
        .publish(
            &graph(),
            &store.leader().unwrap(),
            baleyg::model::IndexPin {
                index_generation: store.status().unwrap().revision.index_generation,
                index_revision: 0,
            },
            &cancel(),
        )
        .unwrap();
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
    let (state, _work, store) = fixture();
    let baseline = store.status().unwrap().revision;
    store
        .publish(&graph(), &store.leader().unwrap(), baseline, &cancel())
        .unwrap();
    let previous = store.status().unwrap().revision;
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
    let status = store.status();
    match status {
        Ok(status) => assert_eq!(status.revision, previous),
        Err(e) => assert!(e.to_string().contains("storage_busy"), "{e:#}"),
    }
    let source = store.source_at("a.js", Some(previous));
    match source {
        Ok(Some((pin, source))) => {
            assert_eq!(pin, previous);
            assert_eq!(source.text, "function a() {}");
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
    use baleyg::indexer::{IndexOptions, index_workspace_with_capture};
    let (state, work, store) = fixture();
    std::fs::write(
        work.path().join("a.js"),
        "function one() { console.log('measured'); }",
    )
    .unwrap();
    let options = IndexOptions::new(work.path().to_owned());
    let (graph, capture) = index_workspace_with_capture(&options, &cancel(), |_| {}).unwrap();
    let first = store
        .publish_captured(
            &graph,
            &capture,
            &store.leader().unwrap(),
            store.index_baseline().unwrap(),
            &cancel(),
        )
        .unwrap();
    let path = index_db(state.path());
    drop(store);
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute(
            "UPDATE index_metadata SET schema_version=4,extractor_version='native-v1'",
            [],
        )
        .unwrap();
        db.pragma_update(None, "user_version", 4).unwrap();
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
    let rejected = store.publish_captured(
        &graph,
        &capture,
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
            .publish_captured(&graph, &capture, &store.leader().unwrap(), stale, &cancel())
            .unwrap_err()
            .to_string()
            .starts_with("revision conflict")
    );
    assert_old();
    let (_other_state, _other_work, other_store) = fixture();
    assert!(
        store
            .publish_captured(
                &graph,
                &capture,
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
        .publish_captured(&graph, &capture, &store.leader().unwrap(), first, &cancel())
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
            .publish_captured(&graph, &capture, &store.leader().unwrap(), first, &cancel())
            .unwrap_err()
            .to_string()
            .starts_with("revision conflict")
    );
}

#[test]
fn legacy_unknown_trigger_is_refused_before_any_rebaseline_write_or_forged_export() {
    use baleyg::indexer::{IndexOptions, index_workspace_with_capture};
    let (state, work, store) = fixture();
    std::fs::write(work.path().join("a.js"), "function foo() { bar(); }\n").unwrap();
    let options = IndexOptions::new(work.path().to_owned());
    let (graph, capture) = index_workspace_with_capture(&options, &cancel(), |_| {}).unwrap();
    let first = store
        .publish_captured(
            &graph,
            &capture,
            &store.leader().unwrap(),
            store.index_baseline().unwrap(),
            &cancel(),
        )
        .unwrap();
    let path = index_db(state.path());
    let leader = store.leader().unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE index_metadata SET schema_version=4,extractor_version='native-v1'",
        [],
    )
    .unwrap();
    db.pragma_update(None, "user_version", 4).unwrap();
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
        .publish_captured(&graph, &capture, &leader, first, &cancel())
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
    let before = store
        .publish(
            &graph(),
            &store.leader().unwrap(),
            store.status().unwrap().revision,
            &cancel(),
        )
        .unwrap();
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
            .publish(&graph(), &leader, before, &cancel())
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
    use baleyg::indexer::{IndexOptions, index_workspace_with_capture};
    let (state, work, store) = fixture();
    std::fs::write(work.path().join("a.js"), "function one() { foo(); }\n").unwrap();
    let options = IndexOptions::new(work.path().to_owned());
    let (graph, capture) = index_workspace_with_capture(&options, &cancel(), |_| {}).unwrap();
    let old = store
        .publish_captured(
            &graph,
            &capture,
            &store.leader().unwrap(),
            store.index_baseline().unwrap(),
            &cancel(),
        )
        .unwrap();
    let path = index_db(state.path());
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE index_metadata SET schema_version=4,extractor_version='native-v1'",
        [],
    )
    .unwrap();
    db.pragma_update(None, "user_version", 4).unwrap();
    drop(db);
    let old_bytes = std::fs::read(&path).unwrap();
    let leader = store.leader().unwrap();
    let moved = work.path().with_extension("temporarily-moved");
    std::fs::rename(work.path(), &moved).unwrap();
    let refused = store
        .publish_captured(&graph, &capture, &leader, old, &cancel())
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
    use baleyg::indexer::{IndexOptions, index_workspace_with_capture};
    let (state, work, store) = fixture();
    let source = work.path().join("a.js");
    std::fs::write(&source, "function one() { foo(); }\n").unwrap();
    let options = IndexOptions::new(work.path().to_owned());
    let (graph, capture) = index_workspace_with_capture(&options, &cancel(), |_| {}).unwrap();
    let old = store
        .publish_captured(
            &graph,
            &capture,
            &store.leader().unwrap(),
            store.index_baseline().unwrap(),
            &cancel(),
        )
        .unwrap();
    let path = index_db(state.path());
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE index_metadata SET schema_version=4,extractor_version='native-v1'",
        [],
    )
    .unwrap();
    db.pragma_update(None, "user_version", 4).unwrap();
    drop(db);
    let old_bytes = std::fs::read(&path).unwrap();
    let leader = store.leader().unwrap();
    // Capture verifies again after all projection INSERTs and metadata UPDATE.
    std::fs::write(&source, "function changed() { notInCapture(); }\n").unwrap();
    let refused = store
        .publish_captured(&graph, &capture, &leader, old, &cancel())
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
