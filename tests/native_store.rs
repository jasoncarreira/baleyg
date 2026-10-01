use baleyg::{
    indexer::{IndexOptions, index_workspace_bundle},
    model::{CancelFlag, IndexPin},
    store::Store,
};
use rusqlite::Connection;
use std::{
    fs,
    sync::{Arc, atomic::AtomicBool},
};

fn fixture() -> (tempfile::TempDir, tempfile::TempDir, Store, CancelFlag) {
    let state = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    for (name, text) in [
        (
            "flow.js",
            "function hello(x) { if (x) { obj.go(); } }
",
        ),
        (
            "flow.java",
            "class Demo { void go() { if (true) { run(); } } }
",
        ),
        (
            "flow.rs",
            "fn main() { if true { run(); } }
",
        ),
        (
            "flow.py",
            "def go():
    if True:
        run()
",
        ),
    ] {
        fs::write(root.path().join(name), text).unwrap();
    }
    let store = Store::open_for_tests(state.path(), root.path()).unwrap();
    (state, root, store, Arc::new(AtomicBool::new(false)))
}
fn publish(
    store: &Store,
    root: &std::path::Path,
    cancel: &CancelFlag,
    expected: IndexPin,
    leader: &baleyg::store::topology::LeaderGuard,
) -> anyhow::Result<IndexPin> {
    let (graph, native, capture) = index_workspace_bundle(
        &IndexOptions::new(root.to_owned()),
        store.root_id(),
        cancel,
        |_| {},
    )?;
    assert_eq!(capture.graph_projection_count(), 1);
    assert!(
        capture
            .source_operations
            .values()
            .all(|operations| operations.opens == 1
                && operations.complete_reads == 1
                && operations.hashes == 1)
    );
    let revision = store.publish_native(&graph, &capture, &native, leader, expected, cancel)?;
    assert_eq!(capture.graph_projection_count(), 1);
    Ok(revision)
}
fn assert_current_corruption(error: anyhow::Error) {
    assert_eq!(
        error.to_string(),
        "incompatible_index: reconciliation required after invalid current index"
    );
}
#[test]
fn four_languages_normalized_rows_and_pinned_bytes_are_coherent() {
    let (state, root, store, cancel) = fixture();
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    let baseline = store.index_baseline().unwrap();
    let leader = store.leader().unwrap();
    let pin = publish(&store, root.path(), &cancel, baseline, &leader).unwrap();
    assert_ne!(baseline.index_generation, pin.index_generation);
    assert_eq!(
        store.status().unwrap().evidence_format.as_deref(),
        Some("terminal-native-graph-v1")
    );
    let identity =
        baleyg::store::topology::WorkspaceIdentity::discover(Some(root.path()), root.path())
            .unwrap();
    let path = state
        .path()
        .join("cache/indexes")
        .join(&identity.root_key)
        .join("index.db");
    let before_gc = fs::read(&path).unwrap();
    let roots = baleyg::store::topology::TopologyRoots::isolated_for_tests(
        state.path().join("cache"),
        state.path().join("data"),
    );
    let report = roots.gc_report().unwrap();
    assert_eq!(
        (report.derived[0].status, report.derived[0].reason),
        ("busy", "use_lock_busy")
    );
    assert_eq!(fs::read(&path).unwrap(), before_gc);
    let (graph, artifact, capture) = index_workspace_bundle(
        &IndexOptions::new(root.path().to_owned()),
        store.root_id(),
        &cancel,
        |_| {},
    )
    .unwrap();
    for document in &artifact.revision.documents {
        let (stored, bytes) = store.native_source_at(pin, &document.key).unwrap().unwrap();
        assert_eq!(stored, *document);
        assert_eq!(
            bytes,
            capture
                .files
                .iter()
                .find(|f| f.path == document.key.path)
                .unwrap()
                .text
                .as_bytes()
        );
        let coverage = store
            .native_coverage_at(pin, &document.key)
            .unwrap()
            .unwrap();
        assert_eq!(
            coverage,
            *artifact
                .coverage
                .iter()
                .find(|c| c.document_path == document.key.path)
                .unwrap()
        );
        let (_, source) = store
            .source_at(&document.key.path, Some(pin))
            .unwrap()
            .unwrap();
        assert_eq!(source.text.as_bytes(), bytes);
    }
    for declaration in &artifact.declarations {
        if let Some(lookup) = &declaration.lookup_key {
            let found = store
                .native_declarations_at(pin, &declaration.document.language, lookup)
                .unwrap();
            assert!(
                found.contains(declaration),
                "missing typed declaration: {}",
                declaration.syntax_id
            );
        }
        assert_eq!(
            store.native_calls_at(pin, &declaration.syntax_id).unwrap(),
            artifact
                .calls
                .iter()
                .filter(|c| c.owner_syntax_id == declaration.syntax_id)
                .cloned()
                .collect::<Vec<_>>()
        );
        assert_eq!(
            store
                .native_control_regions_at(pin, &declaration.syntax_id)
                .unwrap(),
            artifact
                .control_regions
                .iter()
                .filter(|r| r.owner_syntax_id == declaration.syntax_id)
                .cloned()
                .collect::<Vec<_>>()
        );
    }
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        7
    );
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name LIKE 'native_%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(count >= 18);
    for node in &graph.nodes {
        let parent: Option<String> = db
            .query_row(
                "SELECT owner_syntax_id FROM native_declarations WHERE syntax_id=?1",
                [&node.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            parent, node.parent,
            "native parent differs from measured graph parent"
        );
    }
    assert_eq!(
        store
            .publish(&graph, &leader, pin, &cancel)
            .unwrap_err()
            .to_string(),
        "native_evidence_required: graph-only publication refused"
    );
    let closed = store
        .native_source_at(baseline, &artifact.revision.documents[0].key)
        .unwrap_err();
    assert_eq!(closed.to_string(), "revision conflict: stale native pin");
    drop(db);
    let next = publish(&store, root.path(), &cancel, pin, &leader).unwrap();
    assert_eq!(next.index_generation, pin.index_generation);
    assert_eq!(next.index_revision, pin.index_revision + 1);
    assert!(store.native_declarations_at(pin, "java", "Demo").is_err());
}

#[test]
fn invalid_native_and_conflict_never_change_published_pair() {
    let (state, root, store, cancel) = fixture();
    let baseline = store.index_baseline().unwrap();
    let leader = store.leader().unwrap();
    let pin = publish(&store, root.path(), &cancel, baseline, &leader).unwrap();
    let path = state
        .path()
        .join("cache/indexes")
        .join(
            baleyg::store::topology::WorkspaceIdentity::discover(Some(root.path()), root.path())
                .unwrap()
                .root_key,
        )
        .join("index.db");
    let before = fs::read(&path).unwrap();
    let (graph, mut native, capture) = index_workspace_bundle(
        &IndexOptions::new(root.path().to_owned()),
        store.root_id(),
        &cancel,
        |_| {},
    )
    .unwrap();
    native.provenance[0].derived_from = Some(serde_json::json!({"forged":true}));
    assert!(
        store
            .publish_native(&graph, &capture, &native, &leader, pin, &cancel)
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    native.provenance[0].derived_from = None;
    let mut forged_graph = graph.clone();
    forged_graph.nodes[0].name = "not-source-measured".into();
    assert!(
        store
            .publish_native(&forged_graph, &capture, &native, &leader, pin, &cancel)
            .unwrap_err()
            .to_string()
            .contains("graph declaration differs from measured native row")
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(
        store
            .publish_native(&graph, &capture, &native, &leader, baseline, &cancel)
            .unwrap_err()
            .to_string()
            .contains("revision conflict")
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    let mut value = serde_json::to_value(&native).unwrap();
    value["calls"][0]["target"] = serde_json::json!("unmeasured");
    assert!(serde_json::from_value::<baleyg::native_evidence::Artifact>(value).is_err());
    assert_eq!(store.index_baseline().unwrap(), pin);
    assert_eq!(store.status().unwrap().revision, pin);
    assert_eq!(store.graph().unwrap(), graph);
    for document in &native.revision.documents {
        let (stored, bytes) = store.native_source_at(pin, &document.key).unwrap().unwrap();
        assert_eq!(stored, *document);
        assert_eq!(
            bytes,
            capture
                .files
                .iter()
                .find(|file| file.path == document.key.path)
                .unwrap()
                .text
                .as_bytes()
        );
    }
}

#[test]
fn empty_workspace_has_native_pair_without_document_rows() {
    let state = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let store = Store::open_for_tests(state.path(), root.path()).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let original = store.index_baseline().unwrap();
    let leader = store.leader().unwrap();
    let pin = publish(&store, root.path(), &cancel, original, &leader).unwrap();
    assert_eq!(store.status().unwrap().revision, pin);
    let db = Connection::open(
        state
            .path()
            .join("cache/indexes")
            .join(
                baleyg::store::topology::WorkspaceIdentity::discover(
                    Some(root.path()),
                    root.path(),
                )
                .unwrap()
                .root_key,
            )
            .join("index.db"),
    )
    .unwrap();
    for table in [
        "native_documents",
        "native_coverage",
        "native_provenance",
        "native_calls",
        "native_declarations",
    ] {
        let count: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    assert_eq!(
        db.query_row("SELECT count(*) FROM native_revisions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn known_old_schema_four_remains_unready_until_full_native_reindex() {
    let (state, root, store, cancel) = fixture();
    let original = store.index_baseline().unwrap();
    let path = state
        .path()
        .join("cache/indexes")
        .join(
            baleyg::store::topology::WorkspaceIdentity::discover(Some(root.path()), root.path())
                .unwrap()
                .root_key,
        )
        .join("index.db");
    let db = Connection::open(&path).unwrap();
    db.execute(
        "UPDATE index_metadata SET schema_version=4,extractor_version='native-v1'",
        [],
    )
    .unwrap();
    db.pragma_update(None, "user_version", 4).unwrap();
    drop(db);
    assert_eq!(store.index_baseline().unwrap(), original);
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    let leader = store.leader().unwrap();
    let before = fs::read(&path).unwrap();
    let (graph, native, capture) = index_workspace_bundle(
        &IndexOptions::new(root.path().to_owned()),
        store.root_id(),
        &cancel,
        |_| {},
    )
    .unwrap();
    cancel.store(true, std::sync::atomic::Ordering::Release);
    assert!(
        store
            .publish_native(&graph, &capture, &native, &leader, original, &cancel)
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    cancel.store(false, std::sync::atomic::Ordering::Release);
    let pin = store
        .publish_native(&graph, &capture, &native, &leader, original, &cancel)
        .unwrap();
    assert_ne!(pin.index_generation, original.index_generation);
    assert_eq!(pin.index_revision, original.index_revision + 1);
    assert_eq!(store.status().unwrap().revision, pin);
    assert!(
        store
            .native_source_at(original, &native.revision.documents[0].key)
            .is_err()
    );
}

#[test]
fn metadata_status_and_selected_source_reads_do_not_conflate_other_documents() {
    let (state, root, store, cancel) = fixture();
    let leader = store.leader().unwrap();
    let pin = publish(
        &store,
        root.path(),
        &cancel,
        store.index_baseline().unwrap(),
        &leader,
    )
    .unwrap();
    let path = state
        .path()
        .join("cache/indexes")
        .join(
            baleyg::store::topology::WorkspaceIdentity::discover(Some(root.path()), root.path())
                .unwrap()
                .root_key,
        )
        .join("index.db");
    let db = Connection::open(path).unwrap();
    let source_set_id = format!("source-set:v1:{}", store.root_id());
    let unaffected = baleyg::native_evidence::DocumentKey {
        source_set_id: source_set_id.clone(),
        language: "java".into(),
        path: "flow.java".into(),
    };
    let corrupted = baleyg::native_evidence::DocumentKey {
        source_set_id,
        language: "rust".into(),
        path: "flow.rs".into(),
    };

    // Operational lock contention may fail before the selected query. It must
    // never authorize recovery or close a pre-existing Store clone.
    let operational_clone = store.clone();
    db.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let operational = store.native_source_at(pin, &corrupted).unwrap_err();
    let raw_lock = matches!(
        operational.downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::SqliteFailure(info, _))
            if matches!(
                info.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    );
    assert!(
        operational.to_string().starts_with("storage_busy:") || raw_lock,
        "{operational:#}"
    );
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(operational_clone.status().unwrap().revision, pin);

    let mut bytes: Vec<u8> = db
        .query_row(
            "SELECT source_bytes FROM native_documents WHERE path='flow.rs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    bytes[0] ^= 1;
    db.execute(
        "UPDATE native_documents SET source_bytes=?1 WHERE path='flow.rs'",
        [bytes],
    )
    .unwrap();
    drop(db);
    // Status is a bounded metadata/pin check; selected reads validate the exact stored BLOB.
    assert_eq!(store.status().unwrap().revision, pin);
    assert!(store.native_source_at(pin, &unaffected).unwrap().is_some());
    let original_clone = store.clone();
    let selected = store.native_source_at(pin, &corrupted).unwrap_err();
    assert!(
        selected.to_string().contains("incompatible_index")
            && selected.to_string().contains("native source hash mismatch"),
        "{selected:#}"
    );
    let closed = original_clone.source_at("flow.rs", Some(pin)).unwrap_err();
    assert_current_corruption(closed);

    let reopened = Store::open_for_tests(state.path(), root.path()).unwrap();
    assert_eq!(reopened.status().unwrap().revision, pin);
    assert_eq!(reopened.index_baseline().unwrap(), pin);
    let reopened_clone = reopened.clone();
    let selected = reopened.source_at("flow.rs", Some(pin)).unwrap_err();
    assert!(
        selected.to_string().contains("incompatible_index")
            && selected.to_string().contains("source hash mismatch"),
        "{selected:#}"
    );
    for closed in [
        reopened_clone.status().unwrap_err(),
        reopened.source_at("flow.rs", Some(pin)).unwrap_err(),
    ] {
        assert_current_corruption(closed);
    }
}

#[test]
fn status_many_documents_only_checks_paired_metadata_not_every_blob() {
    let state = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    for n in 0..64 {
        fs::write(
            root.path().join(format!("source{n:02}.js")),
            format!(
                "function item{n}() {{ return {n}; }}\n{}",
                "// captured bytes\n".repeat(256)
            ),
        )
        .unwrap();
    }
    let store = Store::open_for_tests(state.path(), root.path()).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let leader = store.leader().unwrap();
    let pin = publish(
        &store,
        root.path(),
        &cancel,
        store.index_baseline().unwrap(),
        &leader,
    )
    .unwrap();
    let db_path = state
        .path()
        .join("cache/indexes")
        .join(
            baleyg::store::topology::WorkspaceIdentity::discover(Some(root.path()), root.path())
                .unwrap()
                .root_key,
        )
        .join("index.db");
    let db = Connection::open(db_path).unwrap();
    let mut bytes: Vec<u8> = db
        .query_row(
            "SELECT source_bytes FROM native_documents WHERE path='source63.js'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    bytes[0] ^= 1;
    db.execute(
        "UPDATE native_documents SET source_bytes=?1 WHERE path='source63.js'",
        [bytes],
    )
    .unwrap();
    drop(db);
    for _ in 0..64 {
        assert_eq!(store.status().unwrap().revision, pin);
    }
    let key = baleyg::native_evidence::DocumentKey {
        source_set_id: format!("source-set:v1:{}", store.root_id()),
        language: "javascript".into(),
        path: "source63.js".into(),
    };
    assert!(store.native_source_at(pin, &key).is_err());
}

#[test]
fn index_from_another_native_producer_version_is_rebuilt_not_served() {
    let (state, root, store, cancel) = fixture();
    let leader = store.leader().unwrap();
    let first = publish(
        &store,
        root.path(),
        &cancel,
        store.index_baseline().unwrap(),
        &leader,
    )
    .unwrap();
    drop(leader);
    drop(store);
    // An index persisted by the withdrawn occ:v1 producer: its occurrence IDs cannot be
    // reproduced by this binary's occ:v2 derivation.
    let path = published_db(state.path(), root.path());
    let db = Connection::open(&path).unwrap();
    db.execute("UPDATE native_producers SET version='native-v2'", [])
        .unwrap();
    drop(db);
    let store = Store::open_for_tests(state.path(), root.path()).unwrap();
    assert_eq!(store.index_baseline().unwrap(), first);
    let leader = store.leader().unwrap();
    for error in [
        store.status().map(|_| ()).unwrap_err(),
        store.graph().map(|_| ()).unwrap_err(),
        store
            .native_declarations_at(first, "javascript", "hello")
            .map(|_| ())
            .unwrap_err(),
    ] {
        assert!(
            error.to_string().contains("incompatible_index"),
            "{error:#}"
        );
    }
    let pin = publish(&store, root.path(), &cancel, first, &leader).unwrap();
    assert_ne!(pin.index_generation, first.index_generation);
    assert_eq!(store.status().unwrap().revision, pin);
    let db = Connection::open(&path).unwrap();
    let version: String = db
        .query_row("SELECT version FROM native_producers", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, "native-v3");
    let calls: Vec<String> = db
        .prepare("SELECT id FROM native_calls")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(!calls.is_empty() && calls.iter().all(|id| id.starts_with("occ:v2:")));
}

fn published_db(state: &std::path::Path, root: &std::path::Path) -> std::path::PathBuf {
    state
        .join("cache/indexes")
        .join(
            baleyg::store::topology::WorkspaceIdentity::discover(Some(root), root)
                .unwrap()
                .root_key,
        )
        .join("index.db")
}

#[test]
fn selected_typed_rows_reject_constraint_preserving_sql_forgery_at_same_pin() {
    let (state, root, store, cancel) = fixture();
    let leader = store.leader().unwrap();
    let pin = publish(
        &store,
        root.path(),
        &cancel,
        store.index_baseline().unwrap(),
        &leader,
    )
    .unwrap();
    let db = Connection::open(published_db(state.path(), root.path())).unwrap();
    let key = baleyg::native_evidence::DocumentKey {
        source_set_id: format!("source-set:v1:{}", store.root_id()),
        language: "javascript".into(),
        path: "flow.js".into(),
    };
    let owner: String = db
        .query_row(
            "SELECT syntax_id FROM native_declarations WHERE path='flow.js' AND name='hello'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        !store
            .native_declarations_at(pin, "javascript", "hello")
            .unwrap()
            .is_empty()
    );
    assert!(!store.native_calls_at(pin, &owner).unwrap().is_empty());
    assert!(
        !store
            .native_control_regions_at(pin, &owner)
            .unwrap()
            .is_empty()
    );
    assert!(store.native_coverage_at(pin, &key).unwrap().is_some());
    // Every edit preserves row identity, pin, ordinals, ranges and FK constraints.
    let cases = [
        (
            "native_headers",
            "result_type",
            "UPDATE native_headers SET result_type='Fabricated' WHERE syntax_id=(SELECT syntax_id FROM native_declarations WHERE path='flow.js' AND name='hello')",
        ),
        (
            "native_calls",
            "spelling",
            "UPDATE native_calls SET spelling='fabricated' WHERE path='flow.js'",
        ),
        (
            "native_control_regions",
            "arm",
            "UPDATE native_control_regions SET arm='fabricated' WHERE path='flow.js'",
        ),
        (
            "native_coverage",
            "selected",
            "UPDATE native_coverage SET selected=0 WHERE document_path='flow.js'",
        ),
    ];
    for (table, column, sql) in cases {
        let old: rusqlite::types::Value = db.query_row(
            &format!("SELECT {column} FROM {table} WHERE {} LIMIT 1", if table == "native_headers" {
                "syntax_id=(SELECT syntax_id FROM native_declarations WHERE path='flow.js' AND name='hello')"
            } else if table == "native_coverage" { "document_path='flow.js'" } else { "path='flow.js'" }),
            [], |r| r.get(0),
        ).unwrap();
        assert!(db.execute(sql, []).unwrap() > 0, "{table} fixture row");
        let selected_store = Store::open_for_tests(state.path(), root.path()).unwrap();
        assert_eq!(
            selected_store.status().unwrap().revision,
            pin,
            "pin must not explain refusal"
        );
        let selected_clone = selected_store.clone();
        let error = match table {
            "native_headers" => selected_store
                .native_declarations_at(pin, "javascript", "hello")
                .map(|_| ()),
            "native_calls" => selected_store.native_calls_at(pin, &owner).map(|_| ()),
            "native_control_regions" => selected_store
                .native_control_regions_at(pin, &owner)
                .map(|_| ()),
            _ => selected_store.native_coverage_at(pin, &key).map(|_| ()),
        }
        .unwrap_err();
        let expected = match table {
            "native_headers" => "selected native declarations differ",
            "native_calls" => "selected native calls differ",
            "native_control_regions" => "selected native regions differ",
            _ => "selected native coverage differs",
        };
        assert!(
            error.to_string().contains("incompatible_index")
                && error.to_string().contains(expected),
            "{table}.{column}: {error:#}"
        );
        let java = baleyg::native_evidence::DocumentKey {
            source_set_id: key.source_set_id.clone(),
            language: "java".into(),
            path: "flow.java".into(),
        };
        let closed = selected_clone.native_source_at(pin, &java).unwrap_err();
        assert_current_corruption(closed);
        let predicate = if table == "native_headers" {
            "syntax_id=(SELECT syntax_id FROM native_declarations WHERE path='flow.js' AND name='hello')"
        } else if table == "native_coverage" {
            "document_path='flow.js'"
        } else {
            "path='flow.js'"
        };
        db.execute(
            &format!("UPDATE {table} SET {column}=?1 WHERE {predicate}"),
            [old],
        )
        .unwrap();
    }
}

#[test]
fn selected_sources_bind_paired_hash_bytes_and_graph_path_without_pin_change() {
    use sha2::{Digest, Sha256};
    let (state, root, store, cancel) = fixture();
    let leader = store.leader().unwrap();
    let pin = publish(
        &store,
        root.path(),
        &cancel,
        store.index_baseline().unwrap(),
        &leader,
    )
    .unwrap();
    let db = Connection::open(published_db(state.path(), root.path())).unwrap();
    let key = baleyg::native_evidence::DocumentKey {
        source_set_id: format!("source-set:v1:{}", store.root_id()),
        language: "javascript".into(),
        path: "flow.js".into(),
    };
    let (original, bytes) = store.native_source_at(pin, &key).unwrap().unwrap();
    let mut forged = bytes.clone();
    let offset = forged.iter().position(|b| *b == b'o').unwrap();
    forged[offset] = b'x';
    let forged_hash = hex::encode(Sha256::digest(&forged));
    db.execute_batch("BEGIN").unwrap();
    db.execute(
        "UPDATE native_documents SET source_bytes=?1, content_hash=?2 WHERE path='flow.js'",
        rusqlite::params![forged, forged_hash],
    )
    .unwrap();
    db.execute(
        "UPDATE native_provenance SET content_hash=?1 WHERE path='flow.js'",
        [&forged_hash],
    )
    .unwrap();
    db.execute_batch("COMMIT").unwrap();
    assert_eq!(store.status().unwrap().revision, pin);
    let native_clone = store.clone();
    let native_error = store.native_source_at(pin, &key).unwrap_err();
    assert!(
        native_error.to_string().contains("incompatible_index")
            && native_error.to_string().contains("source hash mismatch"),
        "{native_error:#}"
    );
    let closed = native_clone.source_at("flow.java", Some(pin)).unwrap_err();
    assert_current_corruption(closed);

    let graph_store = Store::open_for_tests(state.path(), root.path()).unwrap();
    assert_eq!(graph_store.status().unwrap().revision, pin);
    let graph_clone = graph_store.clone();
    let graph_error = graph_store.source_at("flow.js", Some(pin)).unwrap_err();
    assert!(
        graph_error.to_string().contains("incompatible_index")
            && graph_error.to_string().contains("source hash mismatch"),
        "{graph_error:#}"
    );
    let closed = graph_clone.source_at("flow.java", Some(pin)).unwrap_err();
    assert_current_corruption(closed);

    db.execute_batch("BEGIN").unwrap();
    db.execute(
        "UPDATE native_documents SET source_bytes=?1, content_hash=?2 WHERE path='flow.js'",
        rusqlite::params![bytes, original.content_hash],
    )
    .unwrap();
    db.execute(
        "UPDATE native_provenance SET content_hash=?1 WHERE path='flow.js'",
        [&original.content_hash],
    )
    .unwrap();
    db.execute_batch("COMMIT").unwrap();
    let original_payload: String = db
        .query_row("SELECT payload FROM files WHERE path='flow.js'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let mut altered: serde_json::Value = serde_json::from_str(&original_payload).unwrap();
    altered["path"] = serde_json::json!("forged.js");
    db.execute(
        "UPDATE files SET payload=?1 WHERE path='flow.js'",
        [altered.to_string()],
    )
    .unwrap();
    let payload_store = Store::open_for_tests(state.path(), root.path()).unwrap();
    assert_eq!(payload_store.status().unwrap().revision, pin);
    let payload_clone = payload_store.clone();
    let payload_error = payload_store.source_at("flow.js", Some(pin)).unwrap_err();
    assert!(
        payload_error.to_string().contains("incompatible_index")
            && payload_error.to_string().contains("source bytes mismatch"),
        "{payload_error:#}"
    );
    let closed = payload_clone.source_at("flow.java", Some(pin)).unwrap_err();
    assert_current_corruption(closed);
}

#[test]
fn direct_native_ranges_regions_and_coverage_reject_selected_sql_edits() {
    let (state, root, store, cancel) = fixture();
    let leader = store.leader().unwrap();
    let pin = publish(
        &store,
        root.path(),
        &cancel,
        store.index_baseline().unwrap(),
        &leader,
    )
    .unwrap();
    let db = Connection::open(published_db(state.path(), root.path())).unwrap();
    let owner: String = db
        .query_row(
            "SELECT syntax_id FROM native_declarations WHERE path='flow.js' AND name='hello'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let key = baleyg::native_evidence::DocumentKey {
        source_set_id: format!("source-set:v1:{}", store.root_id()),
        language: "javascript".into(),
        path: "flow.js".into(),
    };
    let original_end: i64 = db
        .query_row(
            "SELECT end_byte FROM native_declarations WHERE syntax_id=?1",
            [&owner],
            |r| r.get(0),
        )
        .unwrap();
    db.execute(
        "UPDATE native_declarations SET end_byte=end_byte+1 WHERE syntax_id=?1",
        [&owner],
    )
    .unwrap();
    let declarations_clone = store.clone();
    let error = store
        .native_declarations_at(pin, "javascript", "hello")
        .unwrap_err();
    assert!(
        error.to_string().contains("incompatible_index")
            && error
                .to_string()
                .contains("selected native declarations differ"),
        "{error:#}"
    );
    assert_current_corruption(declarations_clone.status().unwrap_err());
    db.execute(
        "UPDATE native_declarations SET end_byte=?1 WHERE syntax_id=?2",
        rusqlite::params![original_end, owner],
    )
    .unwrap();
    let original_kind: String = db
        .query_row(
            "SELECT kind FROM native_control_regions WHERE path='flow.js' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    db.execute(
        "UPDATE native_control_regions SET kind='while_statement' WHERE path='flow.js'",
        [],
    )
    .unwrap();
    let regions_store = Store::open_for_tests(state.path(), root.path()).unwrap();
    assert_eq!(regions_store.status().unwrap().revision, pin);
    let regions_clone = regions_store.clone();
    let error = regions_store
        .native_control_regions_at(pin, &owner)
        .unwrap_err();
    assert!(
        error.to_string().contains("incompatible_index")
            && error.to_string().contains("selected native regions differ"),
        "{error:#}"
    );
    assert_current_corruption(regions_clone.status().unwrap_err());
    db.execute(
        "UPDATE native_control_regions SET kind=?1 WHERE path='flow.js'",
        [&original_kind],
    )
    .unwrap();
    let original: (String, Option<String>) = db
        .query_row(
            "SELECT state,diagnostic FROM native_coverage WHERE document_path='flow.js'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    db.execute("UPDATE native_coverage SET state='partial',diagnostic='fabricated' WHERE document_path='flow.js'", []).unwrap();
    let coverage_store = Store::open_for_tests(state.path(), root.path()).unwrap();
    assert_eq!(coverage_store.status().unwrap().revision, pin);
    let coverage_clone = coverage_store.clone();
    let error = coverage_store.native_coverage_at(pin, &key).unwrap_err();
    assert!(
        error.to_string().contains("incompatible_index")
            && error
                .to_string()
                .contains("selected native coverage differs"),
        "{error:#}"
    );
    let closed = coverage_clone
        .native_coverage_at(
            pin,
            &baleyg::native_evidence::DocumentKey {
                source_set_id: key.source_set_id,
                language: "java".into(),
                path: "flow.java".into(),
            },
        )
        .unwrap_err();
    assert_current_corruption(closed);
    db.execute(
        "UPDATE native_coverage SET state=?1,diagnostic=?2 WHERE document_path='flow.js'",
        rusqlite::params![original.0, original.1],
    )
    .unwrap();
}

#[test]
fn pinned_graph_call_payload_must_match_native_before_query_and_sequence() {
    let (state, root, store, cancel) = fixture();
    let leader = store.leader().unwrap();
    let pin = publish(
        &store,
        root.path(),
        &cancel,
        store.index_baseline().unwrap(),
        &leader,
    )
    .unwrap();
    let db = Connection::open(published_db(state.path(), root.path())).unwrap();
    let owner: String = db
        .query_row(
            "SELECT syntax_id FROM native_declarations WHERE path='flow.js' AND name='hello'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let query: baleyg::model::ViewQuery =
        serde_json::from_value(serde_json::json!({"seed":owner})).unwrap();
    assert!(!store.query_view(&query).unwrap().unwrap().calls.is_empty());
    assert!(store.sequence_at(&owner, pin, true).unwrap().is_some());
    let call_id: String = db
        .query_row(
            "SELECT id FROM calls WHERE path='flow.js' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let original: String = db
        .query_row("SELECT payload FROM calls WHERE id=?1", [&call_id], |r| {
            r.get(0)
        })
        .unwrap();
    let original_value: serde_json::Value = serde_json::from_str(&original).unwrap();
    assert!(
        original_value["calleeText"].is_string(),
        "fixture must contain measured calleeText"
    );
    let mut forged = original_value.clone();
    forged["calleeText"] = serde_json::json!("fabricatedCallee");
    db.execute(
        "UPDATE calls SET payload=?1 WHERE id=?2",
        rusqlite::params![forged.to_string(), call_id],
    )
    .unwrap();
    assert_eq!(store.status().unwrap().revision, pin);
    let query_clone = store.clone();
    let query_error = store.query_view(&query).unwrap_err();
    assert!(
        query_error.to_string().contains("native_evidence_required")
            && query_error
                .to_string()
                .contains("graph call differs from measured native row"),
        "{query_error:#}"
    );
    assert_current_corruption(query_clone.status().unwrap_err());

    let sequence_store = Store::open_for_tests(state.path(), root.path()).unwrap();
    assert_eq!(sequence_store.status().unwrap().revision, pin);
    let sequence_clone = sequence_store.clone();
    let sequence_error = sequence_store.sequence_at(&owner, pin, true).unwrap_err();
    assert!(
        sequence_error
            .to_string()
            .contains("native_evidence_required")
            && sequence_error
                .to_string()
                .contains("graph call differs from measured native row"),
        "{sequence_error:#}"
    );
    let java: String = db
        .query_row(
            "SELECT syntax_id FROM native_declarations WHERE path='flow.java' AND name='go'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let unaffected: baleyg::model::ViewQuery =
        serde_json::from_value(serde_json::json!({"seed":java})).unwrap();
    for closed in [
        sequence_clone.query_view(&unaffected).unwrap_err(),
        sequence_store.sequence_at(&java, pin, true).unwrap_err(),
    ] {
        assert_current_corruption(closed);
    }
}

#[test]
fn extra_fk_valid_native_declaration_with_new_lookup_key_cannot_escape_source_witness() {
    let (state, root, store, cancel) = fixture();
    let leader = store.leader().unwrap();
    let pin = publish(
        &store,
        root.path(),
        &cancel,
        store.index_baseline().unwrap(),
        &leader,
    )
    .unwrap();
    let db = Connection::open(published_db(state.path(), root.path())).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    let original: String = db
        .query_row(
            "SELECT syntax_id FROM native_declarations WHERE path='flow.js' AND name='hello'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let fake = "sid:v1:ffffffffffffffffffffffffffffffff";
    db.execute("INSERT INTO native_declarations(syntax_id,source_set_id,language,path,revision_id,owner_syntax_id,kind,name,lookup_key,key_signature_present,key_type_parameter_count,key_variadic,key_ordinal,start_byte,end_byte,name_start,name_end,provenance_id)
        SELECT ?1,source_set_id,language,path,revision_id,owner_syntax_id,kind,'phantom','phantom',key_signature_present,key_type_parameter_count,key_variadic,key_ordinal,start_byte,end_byte,name_start,name_end,provenance_id
        FROM native_declarations WHERE syntax_id=?2",
        rusqlite::params![fake,original]).unwrap();
    db.execute(
        "INSERT INTO native_headers(syntax_id,kind,name,result_type)
        SELECT ?1,kind,'phantom',result_type FROM native_headers WHERE syntax_id=?2",
        rusqlite::params![fake, original],
    )
    .unwrap();
    let fk_count: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fk_count, 0, "forgery must retain valid SQL FKs");
    assert_eq!(store.status().unwrap().revision, pin);
    let selected_clone = store.clone();
    let error = store
        .native_declarations_at(pin, "javascript", "phantom")
        .unwrap_err();
    assert!(
        error.to_string().contains("incompatible_index")
            && error
                .to_string()
                .contains("selected native declaration inventory differs from source"),
        "{error:#}"
    );
    let closed = selected_clone
        .native_declarations_at(pin, "java", "go")
        .unwrap_err();
    assert_current_corruption(closed);
}

#[test]
fn amplified_selected_ancillary_rows_refuse_before_typed_materialization() {
    let (state, root, store, cancel) = fixture();
    let leader = store.leader().unwrap();
    let pin = publish(
        &store,
        root.path(),
        &cancel,
        store.index_baseline().unwrap(),
        &leader,
    )
    .unwrap();
    let db = Connection::open(published_db(state.path(), root.path())).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    let owner: String = db
        .query_row(
            "SELECT syntax_id FROM native_declarations WHERE path='flow.js' AND name='hello'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    db.execute(
        "WITH RECURSIVE n(v) AS (SELECT 1 UNION ALL SELECT v+1 FROM n WHERE v<1500)
        INSERT INTO native_signature_parameter_types(syntax_id,ancestor_ordinal,ordinal,type_name)
        SELECT ?1,10000+v,0,'amplified' FROM n",
        [&owner],
    )
    .unwrap();
    let fk_count: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fk_count, 0, "amplification must retain valid SQL FKs");
    assert_eq!(store.status().unwrap().revision, pin);
    let selected_clone = store.clone();
    let error = store
        .native_declarations_at(pin, "javascript", "hello")
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("selected evidence row budget exceeded"),
        "{error:#}"
    );
    let closed = selected_clone.status().unwrap_err();
    assert_current_corruption(closed);
    let closed = store.native_declarations_at(pin, "java", "go").unwrap_err();
    assert_current_corruption(closed);
}

#[test]
fn single_oversized_fk_valid_native_child_text_refuses_before_materialization() {
    let (state, root, store, cancel) = fixture();
    let leader = store.leader().unwrap();
    let pin = publish(
        &store,
        root.path(),
        &cancel,
        store.index_baseline().unwrap(),
        &leader,
    )
    .unwrap();
    let db = Connection::open(published_db(state.path(), root.path())).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    let owner: String = db
        .query_row(
            "SELECT syntax_id FROM native_declarations WHERE path='flow.js' AND name='hello'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    db.execute("INSERT INTO native_signature_parameter_types(syntax_id,ancestor_ordinal,ordinal,type_name) VALUES(?1,10000,0,?2)",
        rusqlite::params![owner,"X".repeat(64*1024)]).unwrap();
    let fk_count: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fk_count, 0, "single oversized child remains FK-valid");
    assert_eq!(store.status().unwrap().revision, pin);
    let selected_clone = store.clone();
    let error = store
        .native_declarations_at(pin, "javascript", "hello")
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("selected evidence byte budget exceeded"),
        "{error:#}"
    );
    let closed = selected_clone.status().unwrap_err();
    assert_current_corruption(closed);
    let closed = store.native_declarations_at(pin, "java", "go").unwrap_err();
    assert_current_corruption(closed);
}

#[test]
fn selected_class_payload_oversize_refuses_before_json_decode() {
    let (state, root, store, cancel) = fixture();
    let leader = store.leader().unwrap();
    let pin = publish(
        &store,
        root.path(),
        &cancel,
        store.index_baseline().unwrap(),
        &leader,
    )
    .unwrap();
    let db = Connection::open(published_db(state.path(), root.path())).unwrap();
    let id: String = db
        .query_row(
            "SELECT id FROM classes WHERE path='flow.java' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    db.execute(
        "UPDATE classes SET payload=?1 WHERE id=?2",
        rusqlite::params!["not-valid-class-json".repeat(8000), id],
    )
    .unwrap();
    let selected_clone = store.clone();
    let error = store.symbol_at(&id, Some(pin)).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("selected evidence byte budget exceeded"),
        "{error:#}"
    );
    let rust_id: String = db
        .query_row(
            "SELECT syntax_id FROM native_declarations WHERE path='flow.rs' AND name='main'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let closed = selected_clone.symbol_at(&rust_id, Some(pin)).unwrap_err();
    assert_current_corruption(closed);
}

#[test]
fn selected_native_text_total_budget_is_not_count_times_single_row_limit() {
    let (state, root, store, cancel) = fixture();
    let leader = store.leader().unwrap();
    let pin = publish(
        &store,
        root.path(),
        &cancel,
        store.index_baseline().unwrap(),
        &leader,
    )
    .unwrap();
    let db = Connection::open(published_db(state.path(), root.path())).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    let owner: String = db
        .query_row(
            "SELECT syntax_id FROM native_declarations WHERE path='flow.js' AND name='hello'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    db.execute(
        "WITH RECURSIVE n(v) AS (SELECT 1 UNION ALL SELECT v+1 FROM n WHERE v<40)
        INSERT INTO native_signature_parameter_types(syntax_id,ancestor_ordinal,ordinal,type_name)
        SELECT ?1,30000+v,0,?2 FROM n",
        rusqlite::params![owner, "Y".repeat(8 * 1024)],
    )
    .unwrap();
    let fk_count: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fk_count, 0);
    assert_eq!(store.status().unwrap().revision, pin);
    let selected_clone = store.clone();
    let error = store
        .native_declarations_at(pin, "javascript", "hello")
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("selected evidence byte budget exceeded"),
        "{error:#}"
    );
    let closed = selected_clone
        .native_declarations_at(pin, "java", "go")
        .unwrap_err();
    assert_current_corruption(closed);
}

#[test]
fn graph_node_call_and_region_payloads_each_have_predecode_byte_envelope() {
    let (state, root, store, cancel) = fixture();
    let leader = store.leader().unwrap();
    let pin = publish(
        &store,
        root.path(),
        &cancel,
        store.index_baseline().unwrap(),
        &leader,
    )
    .unwrap();
    let db = Connection::open(published_db(state.path(), root.path())).unwrap();
    for table in ["nodes", "calls", "regions"] {
        // These three fixed SQL identifiers are the selected graph row families.
        let query = format!("SELECT id,payload FROM {table} WHERE path='flow.js' LIMIT 1");
        let (id, original): (String, String) = db
            .query_row(&query, [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        db.execute(
            &format!("UPDATE {table} SET payload=?1 WHERE id=?2"),
            rusqlite::params!["invalid-json".repeat(8000), id],
        )
        .unwrap();
        let selected_store = Store::open_for_tests(state.path(), root.path()).unwrap();
        assert_eq!(selected_store.status().unwrap().revision, pin);
        let selected_clone = selected_store.clone();
        let error = selected_store
            .native_declarations_at(pin, "javascript", "hello")
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("selected graph row byte budget exceeded"),
            "{table}: {error:#}"
        );
        let closed = selected_clone
            .native_declarations_at(pin, "java", "go")
            .unwrap_err();
        assert_current_corruption(closed);
        db.execute(
            &format!("UPDATE {table} SET payload=?1 WHERE id=?2"),
            rusqlite::params![original, id],
        )
        .unwrap();
    }
}

#[test]
fn selected_graph_rows_share_a_source_scoped_aggregate_byte_envelope() {
    let (state, root, store, cancel) = fixture();
    let leader = store.leader().unwrap();
    let pin = publish(
        &store,
        root.path(),
        &cancel,
        store.index_baseline().unwrap(),
        &leader,
    )
    .unwrap();
    let db = Connection::open(published_db(state.path(), root.path())).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    db.execute(
        "WITH RECURSIVE n(v) AS (SELECT 1 UNION ALL SELECT v+1 FROM n WHERE v<40)
        INSERT INTO nodes(id,name,path,payload)
        SELECT printf('forged-graph-%d',v),'forged','flow.js',?1 FROM n",
        ["invalid-json".repeat(800)],
    )
    .unwrap();
    let fk_count: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fk_count, 0);
    let selected_clone = store.clone();
    let error = store
        .native_declarations_at(pin, "javascript", "hello")
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("selected graph row byte budget exceeded"),
        "{error:#}"
    );
    let closed = selected_clone
        .native_declarations_at(pin, "java", "go")
        .unwrap_err();
    assert_current_corruption(closed);
}
