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
    let store = Store::open(state.path(), work.path()).unwrap();
    (state, work, store)
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
    }
}
fn call(id: &str, from: &str, to: &str) -> CallSite {
    CallSite {
        id: id.into(),
        caller: from.into(),
        callee_text: to.into(),
        path: "a.js".into(),
        range: SourceRange {
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 1,
            ..SourceRange::default()
        },
        target: Some(to.into()),
        candidate_symbols: vec![],
        resolution: Resolution::Internal,
        ordinal: 0,
        regions: vec![],
        callback_arguments: vec![],
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
    assert_eq!(store.status().unwrap().revision, 0);
    assert_eq!(store.publish(&graph, Some(0), &cancel()).unwrap(), 1);
    let old = store.graph().unwrap();
    let mut duplicate = graph.clone();
    duplicate.nodes.push(node("a"));
    assert!(store.publish(&duplicate, Some(1), &cancel()).is_err());
    assert_eq!(store.graph().unwrap(), old);
    assert!(
        store
            .publish(&Graph::default(), Some(0), &cancel())
            .unwrap_err()
            .to_string()
            .starts_with("revision conflict")
    );
    assert!(
        store
            .publish(&Graph::default(), None, &Arc::new(AtomicBool::new(true)))
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    assert_eq!(store.status().unwrap().revision, 1);
    drop(store);
    let store = Store::open(state.path(), work.path()).unwrap();
    assert_eq!(store.graph().unwrap(), old);
    assert_eq!(
        store.source("a.js").unwrap().unwrap().text,
        "function a() {}"
    );
    assert!(
        store
            .source_at("a.js", Some(0))
            .unwrap_err()
            .to_string()
            .starts_with("revision conflict")
    );
    assert_eq!(store.symbol_at("a", Some(1)).unwrap().unwrap().0, 1);
    assert_eq!(store.symbols_at("", 5).unwrap().0, 1);
    let mut reverse = graph.clone();
    reverse.nodes.reverse();
    reverse.calls.reverse();
    store.publish(&reverse, Some(1), &cancel()).unwrap();
    assert_eq!(store.graph().unwrap(), old);
}
#[test]
fn wal_reader_pins_old_revision_during_publish() {
    let (state, _work, store) = fixture();
    store.publish(&graph(), None, &cancel()).unwrap();
    let db = rusqlite::Connection::open(state.path().join("cache.db")).unwrap();
    db.execute_batch("BEGIN").unwrap();
    let revision = || {
        db.query_row("SELECT revision FROM revision", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    assert_eq!(revision(), 1);
    store
        .publish(&Graph::default(), Some(1), &cancel())
        .unwrap();
    assert_eq!(revision(), 1);
    assert_eq!(
        db.query_row("SELECT count(*) FROM nodes", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    db.execute_batch("COMMIT").unwrap();
    assert_eq!(revision(), 2);
}
#[test]
fn workspace_binding_migrations_and_corruption() {
    let (state, work, store) = fixture();
    drop(store);
    let other = tempfile::tempdir().unwrap();
    assert!(Store::open(state.path(), other.path()).is_err());
    for name in ["cache.db", "workspace.db"] {
        let db = rusqlite::Connection::open(state.path().join(name)).unwrap();
        let original_version: u32 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        db.pragma_update(None, "user_version", 99).unwrap();
        drop(db);
        assert!(
            Store::open(state.path(), work.path())
                .unwrap_err()
                .to_string()
                .contains("future")
        );
        let db = rusqlite::Connection::open(state.path().join(name)).unwrap();
        db.pragma_update(None, "user_version", original_version)
            .unwrap();
    }
    let bad = tempfile::tempdir().unwrap();
    std::fs::write(bad.path().join("cache.db"), "not sqlite").unwrap();
    assert!(Store::open(bad.path(), work.path()).is_err());
    let bad = tempfile::tempdir().unwrap();
    let db = rusqlite::Connection::open(bad.path().join("cache.db")).unwrap();
    db.execute_batch("CREATE TABLE random(x)").unwrap();
    drop(db);
    assert!(Store::open(bad.path(), work.path()).is_err());
}
#[test]
fn literal_search_and_snapshot_only_sources() {
    let (_state, work, store) = fixture();
    let mut graph = graph();
    graph.nodes.push(node("a%_' OR 1=1 --"));
    store.publish(&graph, None, &cancel()).unwrap();
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
    let mut external = call("ae", "a", "external");
    external.resolution = Resolution::External;
    external.callback_arguments.push("callback".into());
    graph.calls.push(external.clone());
    store.publish(&graph, None, &cancel()).unwrap();
    let mut q = query();
    let view = store.query_view(&q).unwrap().unwrap();
    assert_eq!(
        view.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    assert_eq!(view.calls.len(), 2);
    q.include_callbacks = true;
    let view = store.query_view(&q).unwrap().unwrap();
    assert!(view.nodes.iter().any(|n| n.id == "callback"));
    assert!(!view.nodes.iter().any(|n| n.id == "external"));
    assert_eq!(view.calls.iter().find(|c| c.id == "ae"), Some(&external));
    q.depth = 5;
    assert_eq!(store.query_view(&q).unwrap().unwrap().nodes.len(), 4);
    q.max_nodes = 1;
    let bounded = store.query_view(&q).unwrap().unwrap();
    assert_eq!(bounded.nodes.len(), 1);
    assert!(bounded.truncated);
    assert!(!bounded.warnings.is_empty());
    q.max_nodes = 150;
    q.max_calls = 1;
    let bounded = store.query_view(&q).unwrap().unwrap();
    assert_eq!(bounded.calls.len(), 1);
    assert!(bounded.truncated);
    q.exclude_paths.push("a.js".into());
    let view = store.query_view(&q).unwrap().unwrap();
    assert_eq!(view.nodes.len(), 1);
    assert!(view.calls.is_empty());
    q.seed = "missing".into();
    assert!(store.query_view(&q).unwrap().is_none());
    q.depth = 6;
    assert!(store.query_view(&q).is_err());
}
#[test]
fn durable_user_data_survives_cache_loss_and_resolves_orphans() {
    let (state, work, store) = fixture();
    store.publish(&graph(), None, &cancel()).unwrap();
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
    std::fs::remove_file(state.path().join("cache.db")).unwrap();
    let store = Store::open(state.path(), work.path()).unwrap();
    assert_eq!(store.status().unwrap().revision, 0);
    assert!(store.annotations().unwrap()[0].orphaned);
    assert_eq!(store.annotations().unwrap()[0].annotation, annotation);
    assert_eq!(
        store.view("view").unwrap().unwrap().orphaned_ids,
        vec!["a", "b", "missing"]
    );
    store.publish(&graph(), None, &cancel()).unwrap();
    assert!(!store.annotations().unwrap()[0].orphaned);
    store.publish(&Graph::default(), None, &cancel()).unwrap();
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
    store.publish(&graph(), None, &cancel()).unwrap();
    let mut large = Graph {
        files: graph().files,
        ..Graph::default()
    };
    large.nodes = (0..30_000).map(|i| node(&format!("node-{i:05}"))).collect();
    let flag = cancel();
    let worker_flag = flag.clone();
    let worker_store = store.clone();
    let db = rusqlite::Connection::open(state.path().join("cache.db")).unwrap();
    db.busy_timeout(std::time::Duration::ZERO).unwrap();
    let worker = std::thread::spawn(move || worker_store.publish(&large, Some(1), &worker_flag));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
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
        assert!(
            std::time::Instant::now() < deadline,
            "publisher never acquired write transaction"
        );
        std::thread::yield_now();
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
    assert_eq!(store.status().unwrap().revision, 1);
    assert_eq!(store.graph().unwrap().nodes.len(), 3);
}

#[test]
fn concurrent_publish_cas_has_one_winner() {
    let (_state, _work, store) = fixture();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.publish(&graph(), Some(0), &cancel())
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
    );
    assert_eq!(store.status().unwrap().revision, 1);
}

#[test]
fn malformed_graph_rolls_back_and_structural_stats_are_recounted() {
    let (_state, _work, store) = fixture();
    store.publish(&graph(), None, &cancel()).unwrap();
    let baseline = store.graph().unwrap();
    assert_eq!(baseline.stats.symbols, 3);
    assert_eq!(baseline.stats.internal, 3);
    for kind in 0..7 {
        let mut bad = graph();
        match kind {
            0 => bad.calls[0].caller = "missing".into(),
            1 => bad.calls[0].target = Some("missing".into()),
            2 => bad.calls[0].regions.push("missing".into()),
            3 => bad.calls[0].callback_arguments.push("missing".into()),
            4 => bad.nodes[0].parent = Some("missing".into()),
            5 => bad.nodes[0].range.end_byte = usize::MAX,
            _ => bad.nodes[0].range.start_line = 0,
        }
        assert!(
            store.publish(&bad, Some(1), &cancel()).is_err(),
            "case {kind}"
        );
        assert_eq!(store.status().unwrap().revision, 1);
        assert_eq!(store.graph().unwrap(), baseline);
    }
}
#[test]
fn cache_loss_never_reuses_revision_tokens_and_sql_enforces_foreign_keys() {
    let (state, work, store) = fixture();
    let rev = store.publish(&graph(), Some(0), &cancel()).unwrap();
    let db = rusqlite::Connection::open(state.path().join("cache.db")).unwrap();
    db.pragma_update(None, "foreign_keys", true).unwrap();
    assert!(
        db.execute("DELETE FROM files WHERE path='a.js'", [])
            .is_err()
    );
    assert!(db.execute("DELETE FROM nodes WHERE id='a'", []).is_err());
    drop(db);
    drop(store);
    std::fs::remove_file(state.path().join("cache.db")).unwrap();
    let store = Store::open(state.path(), work.path()).unwrap();
    assert_eq!(store.status().unwrap().revision, 0);
    assert!(store.publish(&graph(), Some(rev), &cancel()).is_err());
    let new_rev = store.publish(&graph(), Some(0), &cancel()).unwrap();
    assert!(new_rev > rev);
    assert!(store.source_at("a.js", Some(rev)).is_err());
}
#[test]
fn migrates_v1_preserving_data() {
    let state = private_state();
    let work = tempfile::tempdir().unwrap();
    let db = rusqlite::Connection::open(state.path().join("cache.db")).unwrap();
    db.execute_batch("CREATE TABLE revision(singleton INTEGER PRIMARY KEY, revision INTEGER, indexed_at TEXT, stats TEXT, diagnostics TEXT);
      CREATE TABLE files(path TEXT PRIMARY KEY,hash TEXT,payload TEXT);
      CREATE TABLE nodes(id TEXT PRIMARY KEY,name TEXT,path TEXT,payload TEXT);
      CREATE TABLE calls(id TEXT PRIMARY KEY,caller TEXT,target TEXT,path TEXT,payload TEXT);
      CREATE TABLE regions(id TEXT PRIMARY KEY,owner TEXT,path TEXT,payload TEXT); PRAGMA user_version=1;").unwrap();
    let file = graph().files.remove(0);
    db.execute(
        "INSERT INTO files VALUES(?1,?2,?3)",
        rusqlite::params![file.path, file.hash, serde_json::to_string(&file).unwrap()],
    )
    .unwrap();
    drop(db);
    let db = rusqlite::Connection::open(state.path().join("workspace.db")).unwrap();
    db.execute_batch("CREATE TABLE binding(singleton INTEGER PRIMARY KEY,workspace_root TEXT);
      CREATE TABLE views(id TEXT PRIMARY KEY,payload TEXT);
      CREATE TABLE annotations(id TEXT PRIMARY KEY,node_id TEXT,payload TEXT); PRAGMA user_version=1;").unwrap();
    drop(db);
    let store = Store::open(state.path(), work.path()).unwrap();
    assert_eq!(store.source("a.js").unwrap(), Some(file));
    let db = rusqlite::Connection::open(state.path().join("cache.db")).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM pragma_foreign_key_list('calls')",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(store.publish(&graph(), Some(0), &cancel()).unwrap(), 1);
}

#[test]
fn unresolved_candidate_evidence_need_not_be_a_graph_node() {
    let (_state, _work, store) = fixture();
    let mut graph = graph();
    graph.calls[0].candidate_symbols = vec![
        "external package symbol".into(),
        "local callback parameter".into(),
    ];
    store.publish(&graph, None, &cancel()).unwrap();
    assert_eq!(
        store.graph().unwrap().calls[0].candidate_symbols,
        graph.calls[0].candidate_symbols
    );
}
