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

// Pin every SQL tamper to the admitted revision. Historical versions and
// projections can retain the same source path or measured symbol ID.
fn live_revision(db: &Connection) -> String {
    db.query_row(
        "SELECT r.id FROM native_revisions r JOIN index_metadata m ON m.index_revision=r.published_index_revision",
        [],
        |row| row.get(0),
    ).unwrap()
}
fn live_document_version(db: &Connection, path: &str) -> String {
    db.query_row(
        "SELECT d.document_version_id FROM revision_documents d WHERE d.revision_id=?1 AND d.path=?2",
        rusqlite::params![live_revision(db), path],
        |row| row.get(0),
    ).unwrap()
}
fn live_graph_projection(db: &Connection, path: &str) -> String {
    db.query_row(
        "SELECT d.graph_projection_id FROM revision_documents d WHERE d.revision_id=?1 AND d.path=?2",
        rusqlite::params![live_revision(db), path],
        |row| row.get(0),
    ).unwrap()
}
fn live_class_projection(db: &Connection, path: &str) -> String {
    db.query_row(
        "SELECT d.class_projection_id FROM revision_documents d WHERE d.revision_id=?1 AND d.path=?2",
        rusqlite::params![live_revision(db), path],
        |row| row.get(0),
    ).unwrap()
}
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
    // A current v8 empty bootstrap is the same generation; only a rebuild rotates it.
    assert_eq!(baseline.index_generation, pin.index_generation);
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
        8
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
                "SELECT owner_syntax_id FROM native_version_declarations WHERE version_id=?1 AND syntax_id=?2",
                rusqlite::params![live_document_version(&db, &node.path), node.id],
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
    assert_eq!(
        closed.to_string(),
        "revision conflict: foreign or missing native pin"
    );
    drop(db);
    let next = publish(&store, root.path(), &cancel, pin, &leader).unwrap();
    assert_eq!(next.index_generation, pin.index_generation);
    assert_eq!(next.index_revision, pin.index_revision + 1);
    assert!(
        !store
            .native_declarations_at(pin, "java", "Demo")
            .unwrap()
            .is_empty()
    );
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
        "document_versions",
        "revision_documents",
        "graph_projections",
        "graph_nodes",
        "graph_calls",
        "graph_regions",
        "class_projections",
        "native_version_calls",
        "native_version_declarations",
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
            "SELECT source_bytes FROM document_versions WHERE id=?1",
            [live_document_version(&db, "flow.rs")],
            |r| r.get(0),
        )
        .unwrap();
    bytes[0] ^= 1;
    db.execute(
        "UPDATE document_versions SET source_bytes=?1 WHERE id=?2",
        rusqlite::params![bytes, live_document_version(&db, "flow.rs")],
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
            && selected.to_string().contains(
                "incompatible_index: selected evidence decode failed: incompatible_index: source hash mismatch"
            ),
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
            "SELECT source_bytes FROM document_versions WHERE id=?1",
            [live_document_version(&db, "source63.js")],
            |r| r.get(0),
        )
        .unwrap();
    bytes[0] ^= 1;
    db.execute(
        "UPDATE document_versions SET source_bytes=?1 WHERE id=?2",
        rusqlite::params![bytes, live_document_version(&db, "source63.js")],
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
    for old_version in ["native-v2", "native-v3"] {
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
        // Both withdrawn v2 and immediate v3 predecessor must be refused; v3 used occ:v2 IDs.
        let path = published_db(state.path(), root.path());
        let db = Connection::open(&path).unwrap();
        // Rewrite every producer-version FK in one deferred transaction. This
        // models a coherent index from a withdrawn producer, not a broken FK.
        db.pragma_update(None, "foreign_keys", "ON").unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert_eq!(
            db.execute("UPDATE native_producers SET version=?1", [old_version])
                .unwrap(),
            1
        );
        db.execute(
            "UPDATE native_producer_languages SET producer_version=?1",
            [old_version],
        )
        .unwrap();
        db.execute(
            "UPDATE native_producer_inputs SET producer_version=?1",
            [old_version],
        )
        .unwrap();
        db.execute(
            "UPDATE document_versions SET producer_version=?1",
            [old_version],
        )
        .unwrap();
        db.execute_batch("COMMIT").unwrap();
        let violations: i64 = db
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0, "fixture must preserve producer FKs");
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
        assert_eq!(version, "native-v4");
        let calls: Vec<String> = db
            .prepare("SELECT id FROM native_version_calls")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(!calls.is_empty() && calls.iter().all(|id| id.starts_with("occ:v2:")));
        let old_key = baleyg::native_evidence::DocumentKey {
            source_set_id: format!("source-set:v1:{}", store.root_id()),
            language: "javascript".into(),
            path: "flow.js".into(),
        };
        assert_eq!(
            store
                .native_source_at(first, &old_key)
                .unwrap_err()
                .to_string(),
            "revision conflict: foreign or missing native pin",
            "{old_version} old pin must conflict"
        );
    }
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
            "SELECT syntax_id FROM native_version_declarations WHERE version_id=?1 AND name='hello'",
            [live_document_version(&db, "flow.js")],
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
    let version = live_document_version(&db, "flow.js");
    let revision = live_revision(&db);
    let cases = [
        (
            "native_version_headers",
            "result_type",
            "syntax_id=?3",
            owner.as_str(),
            "Fabricated",
            "selected native declarations differ",
        ),
        (
            "native_version_calls",
            "spelling",
            "owner_syntax_id=?3",
            owner.as_str(),
            "fabricated",
            "selected native calls differ",
        ),
        (
            "native_version_control_regions",
            "arm",
            "owner_syntax_id=?3",
            owner.as_str(),
            "fabricated",
            "selected native regions differ",
        ),
        // Coverage selection belongs to the admitted revision manifest, not to the immutable native version.
        (
            "revision_documents",
            "coverage_selected",
            "path=?3",
            "flow.js",
            "0",
            "selected native coverage differs",
        ),
    ];
    for (table, column, predicate, row_key, forged, expected) in cases {
        let key_column = if table == "revision_documents" {
            "revision_id"
        } else {
            "version_id"
        };
        let parent = if table == "revision_documents" {
            &revision
        } else {
            &version
        };
        let select_predicate = predicate.replace("?3", "?2");
        let where_clause = format!("{key_column}=?1 AND {select_predicate}");
        let old: rusqlite::types::Value = db
            .query_row(
                &format!("SELECT {column} FROM {table} WHERE {where_clause} LIMIT 1"),
                rusqlite::params![parent, row_key],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            db.execute(
                &format!("UPDATE {table} SET {column}=?1 WHERE {key_column}=?2 AND {predicate}"),
                rusqlite::params![forged, parent, row_key],
            )
            .unwrap()
                > 0,
            "{table} fixture row"
        );
        let selected_store = Store::open_for_tests(state.path(), root.path()).unwrap();
        assert_eq!(
            selected_store.status().unwrap().revision,
            pin,
            "pin must not explain refusal"
        );
        let selected_clone = selected_store.clone();
        let error = match table {
            "native_version_headers" => selected_store
                .native_declarations_at(pin, "javascript", "hello")
                .map(|_| ()),
            "native_version_calls" => selected_store.native_calls_at(pin, &owner).map(|_| ()),
            "native_version_control_regions" => selected_store
                .native_control_regions_at(pin, &owner)
                .map(|_| ()),
            _ => selected_store.native_coverage_at(pin, &key).map(|_| ()),
        }
        .unwrap_err();
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
        db.execute(
            &format!("UPDATE {table} SET {column}=?1 WHERE {key_column}=?2 AND {predicate}"),
            rusqlite::params![old, parent, row_key],
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
        "UPDATE document_versions SET source_bytes=?1, content_hash=?2 WHERE id=?3",
        rusqlite::params![forged, forged_hash, live_document_version(&db, "flow.js")],
    )
    .unwrap();
    // Revision-scoped provenance is derived from the manifest and revision header.
    // Do not update a nonexistent native_provenance table.
    db.execute_batch("COMMIT").unwrap();
    assert_eq!(store.status().unwrap().revision, pin);
    let native_clone = store.clone();
    let native_error = store.native_source_at(pin, &key).unwrap_err();
    assert!(
        native_error.to_string().contains("incompatible_index")
            && native_error.to_string().contains("selected native"),
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
            && graph_error.to_string().contains("selected native"),
        "{graph_error:#}"
    );
    let closed = graph_clone.source_at("flow.java", Some(pin)).unwrap_err();
    assert_current_corruption(closed);

    db.execute_batch("BEGIN").unwrap();
    db.execute(
        "UPDATE document_versions SET source_bytes=?1, content_hash=?2 WHERE id=?3",
        rusqlite::params![
            bytes,
            original.content_hash,
            live_document_version(&db, "flow.js")
        ],
    )
    .unwrap();
    // The admitted manifest still names the same immutable document version.
    db.execute_batch("COMMIT").unwrap();
    // v8 has no duplicated files.payload. Forge the selected graph row's
    // serialized path without changing its projection, document version or pin.
    let projection = live_graph_projection(&db, "flow.js");
    assert_eq!(db.execute(
        "UPDATE graph_nodes SET payload=json_set(payload,'$.path','forged.js') WHERE projection_id=?1 AND path='flow.js' AND id=(SELECT id FROM graph_nodes WHERE projection_id=?1 AND path='flow.js' ORDER BY id LIMIT 1)",
        [&projection],
    ).unwrap(), 1);
    let payload_store = Store::open_for_tests(state.path(), root.path()).unwrap();
    assert_eq!(payload_store.status().unwrap().revision, pin);
    let payload_clone = payload_store.clone();
    let payload_error = payload_store.source_at("flow.js", Some(pin)).unwrap_err();
    assert!(
        payload_error.to_string().contains("incompatible_index")
            && !payload_error.to_string().contains("forged.js"),
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
            "SELECT syntax_id FROM native_version_declarations WHERE version_id=?1 AND name='hello'",
            [live_document_version(&db, "flow.js")],
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
            "SELECT end_byte FROM native_version_declarations WHERE version_id=?1 AND syntax_id=?2",
            rusqlite::params![live_document_version(&db, "flow.js"), owner],
            |r| r.get(0),
        )
        .unwrap();
    db.execute(
        "UPDATE native_version_declarations SET end_byte=end_byte+1 WHERE version_id=?1 AND syntax_id=?2",
        rusqlite::params![live_document_version(&db, "flow.js"), owner],
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
        "UPDATE native_version_declarations SET end_byte=?1 WHERE version_id=?2 AND syntax_id=?3",
        rusqlite::params![original_end, live_document_version(&db, "flow.js"), owner],
    )
    .unwrap();
    let original_kind: String = db
        .query_row(
            "SELECT kind FROM native_version_control_regions WHERE version_id=?1 ORDER BY id LIMIT 1",
            [live_document_version(&db, "flow.js")],
            |r| r.get(0),
        )
        .unwrap();
    db.execute(
        "UPDATE native_version_control_regions SET kind='while_statement' WHERE version_id=?1",
        [live_document_version(&db, "flow.js")],
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
        "UPDATE native_version_control_regions SET kind=?1 WHERE version_id=?2",
        rusqlite::params![original_kind, live_document_version(&db, "flow.js")],
    )
    .unwrap();
    let original: (String, Option<String>) = db
        .query_row(
            "SELECT coverage_state,coverage_diagnostic FROM revision_documents WHERE revision_id=?1 AND path='flow.js'",
            [live_revision(&db)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    db.execute("UPDATE revision_documents SET coverage_state='partial',coverage_diagnostic='fabricated' WHERE revision_id=?1 AND path='flow.js'", [live_revision(&db)]).unwrap();
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
        "UPDATE revision_documents SET coverage_state=?1,coverage_diagnostic=?2 WHERE revision_id=?3 AND path='flow.js'",
        rusqlite::params![original.0, original.1, live_revision(&db)],
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
            "SELECT syntax_id FROM native_version_declarations WHERE version_id=?1 AND name='hello'",
            [live_document_version(&db, "flow.js")],
            |r| r.get(0),
        )
        .unwrap();
    let query: baleyg::model::ViewQuery =
        serde_json::from_value(serde_json::json!({"seed":owner})).unwrap();
    assert!(!store.query_view(&query).unwrap().unwrap().calls.is_empty());
    assert!(store.sequence_at(&owner, pin, true).unwrap().is_some());
    let call_id: String = db
        .query_row(
            "SELECT id FROM graph_calls WHERE projection_id=?1 ORDER BY id LIMIT 1",
            [live_graph_projection(&db, "flow.js")],
            |r| r.get(0),
        )
        .unwrap();
    let original: String = db
        .query_row(
            "SELECT payload FROM graph_calls WHERE projection_id=?1 AND id=?2",
            rusqlite::params![live_graph_projection(&db, "flow.js"), call_id],
            |r| r.get(0),
        )
        .unwrap();
    let original_value: serde_json::Value = serde_json::from_str(&original).unwrap();
    assert!(
        original_value["calleeText"].is_string(),
        "fixture must contain measured calleeText"
    );
    let mut forged = original_value.clone();
    forged["calleeText"] = serde_json::json!("fabricatedCallee");
    db.execute(
        "UPDATE graph_calls SET payload=?1 WHERE projection_id=?2 AND id=?3",
        rusqlite::params![
            forged.to_string(),
            live_graph_projection(&db, "flow.js"),
            call_id
        ],
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
            "SELECT syntax_id FROM native_version_declarations WHERE version_id=?1 AND name='go'",
            [live_document_version(&db, "flow.java")],
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
            "SELECT syntax_id FROM native_version_declarations WHERE version_id=?1 AND name='hello'",
            [live_document_version(&db, "flow.js")],
            |r| r.get(0),
        )
        .unwrap();
    let fake = "sid:v1:ffffffffffffffffffffffffffffffff";
    let version = live_document_version(&db, "flow.js");
    db.execute("INSERT INTO native_version_declarations(version_id,syntax_id,owner_syntax_id,kind,name,lookup_key,key_signature_present,key_type_parameter_count,key_variadic,key_ordinal,start_byte,end_byte,name_start,name_end)
        SELECT version_id,?1,owner_syntax_id,kind,'phantom','phantom',key_signature_present,key_type_parameter_count,key_variadic,key_ordinal,start_byte,end_byte,name_start,name_end
        FROM native_version_declarations WHERE version_id=?2 AND syntax_id=?3",
        rusqlite::params![fake,version,original]).unwrap();
    db.execute(
        "INSERT INTO native_version_headers(version_id,syntax_id,kind,name,result_type)
        SELECT version_id,?1,kind,'phantom',result_type FROM native_version_headers WHERE version_id=?2 AND syntax_id=?3",
        rusqlite::params![fake, version, original],
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
                .contains("incompatible_index: selected evidence decode failed: incompatible_index: selected native declarations differ from source"),
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
            "SELECT syntax_id FROM native_version_declarations WHERE version_id=?1 AND name='hello'",
            [live_document_version(&db, "flow.js")],
            |r| r.get(0),
        )
        .unwrap();
    db.execute(
        "WITH RECURSIVE n(v) AS (SELECT 1 UNION ALL SELECT v+1 FROM n WHERE v<1500)
        INSERT INTO native_version_header_items(version_id,syntax_id,item_kind,ordinal,value)
        SELECT ?1,?2,'modifier',10000+v,'amplified' FROM n",
        rusqlite::params![live_document_version(&db, "flow.js"), owner],
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
            "SELECT syntax_id FROM native_version_declarations WHERE version_id=?1 AND name='hello'",
            [live_document_version(&db, "flow.js")],
            |r| r.get(0),
        )
        .unwrap();
    db.execute("INSERT INTO native_version_header_items(version_id,syntax_id,item_kind,ordinal,value) VALUES(?1,?2,'modifier',10000,?3)",
        rusqlite::params![live_document_version(&db, "flow.js"),owner,"X".repeat(64*1024)]).unwrap();
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
            "SELECT id FROM classes WHERE projection_id=?1 ORDER BY id LIMIT 1",
            [live_class_projection(&db, "flow.java")],
            |r| r.get(0),
        )
        .unwrap();
    db.execute(
        "UPDATE classes SET payload=?1 WHERE projection_id=?2 AND id=?3",
        rusqlite::params![
            "not-valid-class-json".repeat(8000),
            live_class_projection(&db, "flow.java"),
            id
        ],
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
            "SELECT syntax_id FROM native_version_declarations WHERE version_id=?1 AND name='main'",
            [live_document_version(&db, "flow.rs")],
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
            "SELECT syntax_id FROM native_version_declarations WHERE version_id=?1 AND name='hello'",
            [live_document_version(&db, "flow.js")],
            |r| r.get(0),
        )
        .unwrap();
    db.execute(
        "WITH RECURSIVE n(v) AS (SELECT 1 UNION ALL SELECT v+1 FROM n WHERE v<40)
        INSERT INTO native_version_header_items(version_id,syntax_id,item_kind,ordinal,value)
        SELECT ?1,?2,'modifier',30000+v,?3 FROM n",
        rusqlite::params![
            live_document_version(&db, "flow.js"),
            owner,
            "Y".repeat(8 * 1024)
        ],
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
    for table in ["graph_nodes", "graph_calls", "graph_regions"] {
        // These three fixed SQL identifiers are the selected graph row families.
        let projection = live_graph_projection(&db, "flow.js");
        let query =
            format!("SELECT id,payload FROM {table} WHERE projection_id=?1 ORDER BY id LIMIT 1");
        let (id, original): (String, String) = db
            .query_row(&query, [&projection], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        db.execute(
            &format!("UPDATE {table} SET payload=?1 WHERE projection_id=?2 AND id=?3"),
            rusqlite::params!["invalid-json".repeat(8000), projection, id],
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
            &format!("UPDATE {table} SET payload=?1 WHERE projection_id=?2 AND id=?3"),
            rusqlite::params![original, projection, id],
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
        INSERT INTO graph_nodes(projection_id,id,name,path,payload)
        SELECT ?1,printf('forged-graph-%d',v),'forged','flow.js',?2 FROM n",
        rusqlite::params![
            live_graph_projection(&db, "flow.js"),
            "invalid-json".repeat(800)
        ],
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

fn assert_retained_native_family_parity(
    store: &Store,
    pin: IndexPin,
    measured: &baleyg::native_evidence::Artifact,
) {
    use std::collections::BTreeSet;

    assert!(
        measured
            .calls
            .iter()
            .any(|call| call.document.path == "flow.js")
    );
    assert!(
        measured
            .control_regions
            .iter()
            .any(|region| region.document.path == "flow.java")
    );
    let lookups = measured
        .declarations
        .iter()
        .filter_map(|declaration| {
            declaration
                .lookup_key
                .as_ref()
                .map(|key| (declaration.document.language.clone(), key.clone()))
        })
        .collect::<BTreeSet<_>>();
    for (language, lookup) in lookups {
        let mut actual = store
            .native_declarations_at(pin, &language, &lookup)
            .unwrap();
        let mut expected = measured
            .declarations
            .iter()
            .filter(|declaration| {
                declaration.document.language == language
                    && declaration.lookup_key.as_deref() == Some(lookup.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        actual.sort_by(|left, right| left.syntax_id.cmp(&right.syntax_id));
        expected.sort_by(|left, right| left.syntax_id.cmp(&right.syntax_id));
        // Whole DTO equality includes the requested document revision and provenance.
        assert_eq!(actual, expected, "{pin:?}: declaration {language}/{lookup}");
    }
    for declaration in &measured.declarations {
        let mut actual_calls = store.native_calls_at(pin, &declaration.syntax_id).unwrap();
        let mut expected_calls = measured
            .calls
            .iter()
            .filter(|call| call.owner_syntax_id == declaration.syntax_id)
            .cloned()
            .collect::<Vec<_>>();
        actual_calls.sort_by(|left, right| left.id.cmp(&right.id));
        expected_calls.sort_by(|left, right| left.id.cmp(&right.id));
        assert_eq!(
            actual_calls, expected_calls,
            "{pin:?}: calls for {}",
            declaration.syntax_id
        );

        let mut actual_regions = store
            .native_control_regions_at(pin, &declaration.syntax_id)
            .unwrap();
        let mut expected_regions = measured
            .control_regions
            .iter()
            .filter(|region| region.owner_syntax_id == declaration.syntax_id)
            .cloned()
            .collect::<Vec<_>>();
        actual_regions.sort_by(|left, right| left.id.cmp(&right.id));
        expected_regions.sort_by(|left, right| left.id.cmp(&right.id));
        assert_eq!(
            actual_regions, expected_regions,
            "{pin:?}: control regions for {}",
            declaration.syntax_id
        );
    }
}

#[test]
fn retained_full_rewrite_pins_survive_edits_delete_release_and_reference_safe_gc() {
    use baleyg::native_evidence::DocumentKey;
    let (state, root, store, cancel) = fixture();
    // Independent full-rewrite measurements, separate from publish()'s capture.
    let (_, measured_r1, _) = index_workspace_bundle(
        &IndexOptions::new(root.path().to_owned()),
        store.root_id(),
        &cancel,
        |_| {},
    )
    .unwrap();
    let leader = store.leader().unwrap();
    let r1 = publish(
        &store,
        root.path(),
        &cancel,
        store.index_baseline().unwrap(),
        &leader,
    )
    .unwrap();
    let key = DocumentKey {
        source_set_id: format!("source-set:v1:{}", store.root_id()),
        language: "javascript".into(),
        path: "flow.js".into(),
    };
    let source1 = store.native_source_at(r1, &key).unwrap().unwrap();
    let coverage1 = store.native_coverage_at(r1, &key).unwrap().unwrap();
    let graph1 = store.graph().unwrap();
    fs::write(
        root.path().join("flow.js"),
        "function changed() { measured(); }\n",
    )
    .unwrap();
    let (_, measured_r2, _) = index_workspace_bundle(
        &IndexOptions::new(root.path().to_owned()),
        store.root_id(),
        &cancel,
        |_| {},
    )
    .unwrap();
    let r2 = publish(&store, root.path(), &cancel, r1, &leader).unwrap();
    let source2 = store.native_source_at(r2, &key).unwrap().unwrap();
    assert_ne!(source1.1, source2.1);
    fs::remove_file(root.path().join("flow.js")).unwrap();
    let r3 = publish(&store, root.path(), &cancel, r2, &leader).unwrap();
    assert!(store.native_source_at(r3, &key).unwrap().is_none());
    assert_eq!(store.native_source_at(r1, &key).unwrap().unwrap(), source1);
    assert_eq!(
        store.native_coverage_at(r1, &key).unwrap().unwrap(),
        coverage1
    );
    assert_eq!(
        store
            .source_at("flow.js", Some(r1))
            .unwrap()
            .unwrap()
            .1
            .text
            .as_bytes(),
        source1.1
    );
    assert_eq!(store.graph_at(Some(r1)).unwrap(), graph1);
    // After both the edit and delete, both old pins must select their own typed
    // native evidence, not the head's or each other's projection.
    assert_retained_native_family_parity(&store, r1, &measured_r1);
    assert_retained_native_family_parity(&store, r2, &measured_r2);
    store.release_revision(r1, &leader).unwrap();
    assert!(
        store
            .native_source_at(r1, &key)
            .unwrap_err()
            .to_string()
            .contains("revision conflict")
    );
    // Release is durable before collection: lose the owner/process now, then
    // reopen the index while r1's unreachable rows still await fenced GC.
    drop(leader);
    drop(store);
    let cold = Store::open_for_tests(state.path(), root.path()).unwrap();
    let cold_leader = cold.leader().unwrap();
    // Taking over the leader lock requires paired publication before public
    // reads, even when the retained rows are intact after the crash window.
    assert!(
        cold.status()
            .unwrap_err()
            .to_string()
            .contains("reconciliation required")
    );

    let identity =
        baleyg::store::topology::WorkspaceIdentity::discover(Some(root.path()), root.path())
            .unwrap();
    let db_path = state
        .path()
        .join("cache/indexes")
        .join(identity.root_key)
        .join("index.db");
    let r2_id = format!("pin:v1:{}:{}", r2.index_generation, r2.index_revision);
    let db = Connection::open(&db_path).unwrap();
    let (version, graph_projection, class_projection): (String, String, String) = db
        .query_row(
            "SELECT document_version_id,graph_projection_id,class_projection_id
             FROM revision_documents WHERE revision_id=?1 AND path='flow.java'",
            [&r2_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    // A surviving revision owns all three materialized record families before
    // GC. Recheck these exact IDs after GC rather than only checking FK health.
    let retained_rows = [
        ("document_versions", version),
        ("graph_projections", graph_projection),
        ("class_projections", class_projection),
    ];
    for (table, id) in &retained_rows {
        let count: i64 = db
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE id=?1"),
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "cold reopen lost retained {table}/{id} before GC");
    }
    drop(db);

    let r4 = publish(&cold, root.path(), &cancel, r3, &cold_leader).unwrap();
    assert_eq!(r4.index_generation, r3.index_generation);
    assert_eq!(r4.index_revision, r3.index_revision + 1);
    assert_eq!(cold.status().unwrap().revision, r4);
    assert!(
        cold.native_source_at(r1, &key)
            .unwrap_err()
            .to_string()
            .contains("revision conflict")
    );
    assert_eq!(cold.native_source_at(r2, &key).unwrap().unwrap(), source2);
    assert!(cold.native_source_at(r3, &key).unwrap().is_none());
    assert_retained_native_family_parity(&cold, r2, &measured_r2);

    cold.collect_unreferenced(&cold_leader).unwrap();
    assert_eq!(cold.native_source_at(r2, &key).unwrap().unwrap(), source2);
    assert!(cold.native_source_at(r3, &key).unwrap().is_none());
    assert_retained_native_family_parity(&cold, r2, &measured_r2);
    let db = Connection::open(&db_path).unwrap();
    for (table, id) in &retained_rows {
        let count: i64 = db
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE id=?1"),
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "fenced GC lost retained {table}/{id}");
    }
    let missing: i64 = db
        .query_row(
            "SELECT count(*) FROM document_versions v WHERE NOT EXISTS
        (SELECT 1 FROM revision_documents m WHERE m.document_version_id=v.id)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(missing, 0);
    let fk: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fk, 0);
    let tombstone: String = db
        .query_row(
            "SELECT payload FROM revision_capture_inputs
        WHERE revision_id=?1 AND input_key='__released:v1'",
            [format!(
                "pin:v1:{}:{}",
                r1.index_generation, r1.index_revision
            )],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tombstone, "released:v1");
    assert_eq!(cold.status().unwrap().revision, r4);
}

#[test]
fn malformed_and_foreign_release_pins_do_not_write_and_header_gaps_remain_corruption() {
    let (state, root, store, cancel) = fixture();
    let leader = store.leader().unwrap();
    let r1 = publish(
        &store,
        root.path(),
        &cancel,
        store.index_baseline().unwrap(),
        &leader,
    )
    .unwrap();
    fs::write(root.path().join("flow.js"), "function next() { next(); }\n").unwrap();
    let r2 = publish(&store, root.path(), &cancel, r1, &leader).unwrap();
    let identity =
        baleyg::store::topology::WorkspaceIdentity::discover(Some(root.path()), root.path())
            .unwrap();
    let path = state
        .path()
        .join("cache/indexes")
        .join(identity.root_key)
        .join("index.db");
    let before = fs::read(&path).unwrap();
    for pin in [
        r2,
        baleyg::model::IndexPin {
            index_generation: uuid::Uuid::new_v4(),
            index_revision: r1.index_revision,
        },
        baleyg::model::IndexPin {
            index_generation: r1.index_generation,
            index_revision: r2.index_revision + 1,
        },
    ] {
        assert!(
            store
                .release_revision(pin, &leader)
                .unwrap_err()
                .to_string()
                .contains("revision conflict")
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            before,
            "invalid release touched the database"
        );
    }
    drop(leader);
    drop(store);
    let db = Connection::open(&path).unwrap();
    db.pragma_update(None, "foreign_keys", "OFF").unwrap();
    db.execute(
        "DELETE FROM revision_documents WHERE revision_id=?1",
        [format!(
            "pin:v1:{}:{}",
            r1.index_generation, r1.index_revision
        )],
    )
    .unwrap();
    db.execute(
        "DELETE FROM revision_capture_inputs WHERE revision_id=?1",
        [format!(
            "pin:v1:{}:{}",
            r1.index_generation, r1.index_revision
        )],
    )
    .unwrap();
    db.execute(
        "DELETE FROM native_revisions WHERE published_index_revision=?1",
        [r1.index_revision as i64],
    )
    .unwrap();
    drop(db);
    let malformed = fs::read(&path).unwrap();
    let reopened = Store::open_for_tests(state.path(), root.path()).unwrap();
    assert!(
        reopened
            .status()
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );
    assert_eq!(
        fs::read(&path).unwrap(),
        malformed,
        "malformed gap was rewritten"
    );
}

#[test]
fn retained_graph_header_preserves_distinct_stats_and_diagnostics_and_matches_live_head_bytes() {
    let (state, root, store, cancel) = fixture();
    let leader = store.leader().unwrap();
    let initial = store.index_baseline().unwrap();
    let publish_measured = |expected| {
        let (graph, native, capture) = index_workspace_bundle(
            &IndexOptions::new(root.path().to_owned()),
            store.root_id(),
            &cancel,
            |_| {},
        )
        .unwrap();
        store
            .publish_native(&graph, &capture, &native, &leader, expected, &cancel)
            .unwrap()
    };
    let r1 = publish_measured(initial);
    let graph1 = store.graph_at(Some(r1)).unwrap();
    // A genuinely incomplete source yields native partial coverage, from which
    // the indexer derives the persisted diagnostic; never invent a graph row.
    fs::write(root.path().join("incomplete.js"), "function broken( {\n").unwrap();
    let r2 = publish_measured(r1);
    let graph2 = store.graph_at(Some(r2)).unwrap();
    assert_ne!(graph1.stats, graph2.stats);
    assert!(
        graph2
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path.as_deref() == Some("incomplete.js"))
    );
    assert_ne!(graph1.diagnostics, graph2.diagnostics);
    assert_eq!(store.graph_at(Some(r1)).unwrap(), graph1);
    assert_eq!(store.graph_at(None).unwrap(), graph2);
    let identity =
        baleyg::store::topology::WorkspaceIdentity::discover(Some(root.path()), root.path())
            .unwrap();
    let db = Connection::open(
        state
            .path()
            .join("cache/indexes")
            .join(identity.root_key)
            .join("index.db"),
    )
    .unwrap();
    let (stats, diagnostics, head_stats, head_diagnostics): (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) = db.query_row(
        "SELECT CAST(r.graph_stats AS BLOB),CAST(r.graph_diagnostics AS BLOB),CAST(m.stats AS BLOB),CAST(m.diagnostics AS BLOB)
         FROM native_revisions r JOIN index_metadata m ON r.id='pin:v1:'||m.index_generation||':'||m.index_revision",
        [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
    ).unwrap();
    assert_eq!(stats, head_stats);
    assert_eq!(diagnostics, head_diagnostics);
}

#[test]
fn invalid_retained_header_json_refuses_old_pin_without_mutating_sqlite_or_sidecars() {
    let (state, root, store, cancel) = fixture();
    let leader = store.leader().unwrap();
    let r1 = publish(
        &store,
        root.path(),
        &cancel,
        store.index_baseline().unwrap(),
        &leader,
    )
    .unwrap();
    fs::write(root.path().join("flow.js"), "function changed() {}\n").unwrap();
    let r2 = publish(&store, root.path(), &cancel, r1, &leader).unwrap();
    let identity =
        baleyg::store::topology::WorkspaceIdentity::discover(Some(root.path()), root.path())
            .unwrap();
    let path = state
        .path()
        .join("cache/indexes")
        .join(identity.root_key)
        .join("index.db");
    drop(store);
    let db = Connection::open(&path).unwrap();
    db.execute(
        "UPDATE native_revisions SET graph_diagnostics='[42]' WHERE published_index_revision=?1",
        [r1.index_revision as i64],
    )
    .unwrap();
    drop(db);
    let bytes = fs::read(&path).unwrap();
    let sidecars: Vec<_> = ["-wal", "-shm", "-journal"]
        .iter()
        .map(|s| path.with_file_name(format!("index.db{s}")))
        .filter(|p| p.exists())
        .collect();
    let reopened = Store::open_for_tests(state.path(), root.path()).unwrap();
    assert_eq!(reopened.status().unwrap().revision, r2);
    assert!(
        reopened
            .graph_at(Some(r1))
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(
        sidecars,
        ["-wal", "-shm", "-journal"]
            .iter()
            .map(|s| path.with_file_name(format!("index.db{s}")))
            .filter(|p| p.exists())
            .collect::<Vec<_>>()
    );
}
