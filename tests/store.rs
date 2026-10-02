mod common;
use baleyg::{model::*, store::Store};
use std::{
    collections::{BTreeMap, BTreeSet},
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
#[test]
fn private_stage_build_is_unpublished_and_cleans_only_its_own_inode() {
    use std::{
        os::unix::fs::{MetadataExt, OpenOptionsExt},
        path::PathBuf,
    };
    let state = private_state();
    let work = tempfile::tempdir().unwrap();
    let mut first_stage: Option<PathBuf> = None;
    let error = Store::open_for_tests_with_index_stage_hook(state.path(), work.path(), |path| {
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("index.db.tmp-")
        );
        assert_eq!(path.parent().unwrap().file_name().unwrap().len(), 64);
        assert!(!path.with_file_name("index.db").exists());
        let named = std::fs::symlink_metadata(path)?;
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let held = opened.metadata()?;
        assert!(named.is_file() && !named.file_type().is_symlink());
        assert_eq!((named.dev(), named.ino()), (held.dev(), held.ino()));
        assert_eq!(held.mode() & 0o777, 0o600);
        assert_eq!(held.nlink(), 1);
        let db = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let metadata_version: i64 = db.query_row(
            "SELECT schema_version FROM index_metadata WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        assert_eq!((version, metadata_version), (8, 8));
        for name in [
            "document_versions",
            "revision_documents",
            "graph_projections",
            "class_projections",
            "native_revisions",
        ] {
            let count: i64 = db.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [name],
                |row| row.get(0),
            )?;
            assert_eq!(count, 1, "missing staged v8 table {name}");
        }
        for name in [
            "class_catalog",
            "files",
            "nodes",
            "calls",
            "regions",
            "native_documents",
            "native_coverage",
            "native_provenance",
        ] {
            let count: i64 = db.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [name],
                |row| row.get(0),
            )?;
            assert_eq!(count, 0, "old table leaked into staged v8: {name}");
        }
        let manifest: i64 =
            db.query_row("SELECT count(*) FROM revision_documents", [], |r| r.get(0))?;
        assert_eq!(manifest, 0, "bootstrap must not publish a partial manifest");
        first_stage = Some(path.to_owned());
        anyhow::bail!("injected before-stage-publication refusal")
    })
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected before-stage-publication refusal")
    );
    let first_stage = first_stage.unwrap();
    assert!(!first_stage.exists());
    assert!(!first_stage.with_file_name("index.db").exists());

    let mut replacement_stage = None;
    let error = Store::open_for_tests_with_index_stage_hook(state.path(), work.path(), |path| {
        assert_ne!(path, first_stage.as_path());
        std::fs::remove_file(path)?;
        std::fs::write(path, b"foreign pathname")?;
        replacement_stage = Some(path.to_owned());
        anyhow::bail!("injected replaced-stage refusal")
    })
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected replaced-stage refusal")
    );
    let replacement_stage = replacement_stage.unwrap();
    assert_eq!(
        std::fs::read(&replacement_stage).unwrap(),
        b"foreign pathname"
    );
    std::fs::remove_file(&replacement_stage).unwrap();
    let store = Store::open_for_tests(state.path(), work.path()).unwrap();
    assert_eq!(store.index_baseline().unwrap().index_revision, 0);
    assert!(!first_stage.exists());
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
// Build a physical schema-4 file from the v8 revision. Lowering markers alone is
// invalid: v8's CHECK constraints and table layout are not the old cache shape.
const LEGACY_V4_SCHEMA: &str = r#"
CREATE TABLE index_metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1), schema_version INTEGER NOT NULL, extractor_version TEXT NOT NULL, root_spelling TEXT NOT NULL, root_device TEXT NOT NULL, root_inode TEXT NOT NULL, index_generation TEXT NOT NULL, index_revision INTEGER NOT NULL CHECK(index_revision BETWEEN 0 AND 9007199254740991), last_opened_at INTEGER NOT NULL CHECK(last_opened_at BETWEEN 0 AND 9007199254740991), indexed_at TEXT NOT NULL, stats TEXT NOT NULL, diagnostics TEXT NOT NULL);
CREATE TABLE files(path TEXT PRIMARY KEY, hash TEXT NOT NULL, payload TEXT NOT NULL);
CREATE TABLE nodes(id TEXT PRIMARY KEY, name TEXT NOT NULL, path TEXT NOT NULL REFERENCES files(path) DEFERRABLE INITIALLY DEFERRED, payload TEXT NOT NULL);
CREATE INDEX nodes_name ON nodes(name);
CREATE TABLE calls(id TEXT PRIMARY KEY, caller TEXT NOT NULL REFERENCES nodes(id) DEFERRABLE INITIALLY DEFERRED, target TEXT, path TEXT NOT NULL REFERENCES files(path) DEFERRABLE INITIALLY DEFERRED, payload TEXT NOT NULL);
CREATE INDEX calls_caller ON calls(caller);
CREATE TABLE regions(id TEXT PRIMARY KEY, owner TEXT NOT NULL REFERENCES nodes(id) DEFERRABLE INITIALLY DEFERRED, path TEXT NOT NULL REFERENCES files(path) DEFERRABLE INITIALLY DEFERRED, payload TEXT NOT NULL);


CREATE TABLE class_catalog(singleton INTEGER PRIMARY KEY CHECK(singleton=1), warnings TEXT NOT NULL, truncated INTEGER NOT NULL);
CREATE TABLE classes(id TEXT PRIMARY KEY REFERENCES nodes(id) DEFERRABLE INITIALLY DEFERRED, name TEXT NOT NULL, qualified_name TEXT NOT NULL, path TEXT NOT NULL, payload TEXT NOT NULL);
CREATE INDEX classes_path ON classes(path,id);
CREATE TABLE class_relations(id TEXT PRIMARY KEY, owner TEXT NOT NULL REFERENCES classes(id) DEFERRABLE INITIALLY DEFERRED, target TEXT REFERENCES classes(id) DEFERRABLE INITIALLY DEFERRED, payload TEXT NOT NULL);
CREATE INDEX class_relations_owner ON class_relations(owner,id);
CREATE INDEX class_relations_target ON class_relations(target,id);
"#;
fn downgrade_to_legacy(db: &rusqlite::Connection) {
    db.execute_batch("PRAGMA foreign_keys=OFF; BEGIN IMMEDIATE;")
        .unwrap();
    // Preserve the exact committed pin and selected graph/class DTOs on the SAME inode.
    // The bootstrap (no published revision) also becomes an empty physical old4 cache.
    db.execute_batch(r#"
        CREATE TEMP TABLE old_metadata AS SELECT singleton,4 AS schema_version,
            'native-v1' AS extractor_version,root_spelling,root_device,root_inode,
            index_generation,index_revision,last_opened_at,indexed_at,stats,diagnostics
            FROM index_metadata;
        CREATE TEMP TABLE old_files AS SELECT d.path,d.content_hash AS hash,
            json_object('path',d.path,'hash',d.content_hash,'language',d.language,
                'text',CAST(d.source_bytes AS TEXT)) AS payload
            FROM revision_documents rd JOIN document_versions d ON d.id=rd.document_version_id;
        CREATE TEMP TABLE old_nodes AS SELECT n.id,n.name,n.path,n.payload
            FROM graph_nodes n JOIN revision_documents rd ON rd.graph_projection_id=n.projection_id;
        CREATE TEMP TABLE old_calls AS SELECT c.id,c.caller,c.target,c.path,c.payload
            FROM graph_calls c JOIN revision_documents rd ON rd.graph_projection_id=c.projection_id;
        CREATE TEMP TABLE old_regions AS SELECT r.id,r.owner,r.path,r.payload
            FROM graph_regions r JOIN revision_documents rd ON rd.graph_projection_id=r.projection_id;
        CREATE TEMP TABLE old_classes AS SELECT c.id,c.name,c.qualified_name,c.path,c.payload
            FROM classes c JOIN revision_documents rd ON rd.class_projection_id=c.projection_id;
        CREATE TEMP TABLE old_class_relations AS SELECT c.id,c.owner,c.target,c.payload
            FROM class_relations c JOIN revision_documents rd ON rd.class_projection_id=c.projection_id;
        CREATE TEMP TABLE old_class_catalog AS SELECT 1 AS singleton,
            COALESCE((SELECT class_warnings FROM native_revisions LIMIT 1),'[]') AS warnings,
            COALESCE((SELECT class_truncated FROM native_revisions LIMIT 1),0) AS truncated;
    "#).unwrap();
    let names = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT GLOB 'sqlite_*'")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for name in names {
        assert!(name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        db.execute_batch(&format!("DROP TABLE \"{name}\";"))
            .unwrap();
    }
    db.execute_batch(LEGACY_V4_SCHEMA).unwrap();
    db.execute_batch(
        r#"
        INSERT INTO index_metadata SELECT * FROM old_metadata;
        INSERT INTO files SELECT * FROM old_files;
        INSERT INTO nodes SELECT * FROM old_nodes;
        INSERT INTO calls SELECT * FROM old_calls;
        INSERT INTO regions SELECT * FROM old_regions;
        INSERT INTO class_catalog SELECT * FROM old_class_catalog;
        INSERT INTO classes SELECT * FROM old_classes;
        INSERT INTO class_relations SELECT * FROM old_class_relations;
        PRAGMA user_version=4;
        COMMIT;
        PRAGMA foreign_keys=ON;
    "#,
    )
    .unwrap();
    let leftovers: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name IN
        ('document_versions','revision_documents','native_revisions','graph_projections')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        leftovers, 0,
        "legacy fixture must be physical old4, never marker-only"
    );
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
    leader: &baleyg::store::topology::LeaderGuard,
) -> IndexPin {
    store
        .publish_native(&bundle.0, &bundle.2, &bundle.1, leader, expected, &cancel())
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
    let leader = store.leader().unwrap();
    let first = publish_bundle(&store, &captured, baseline, &leader);
    assert_eq!(first.index_revision, 1);
    let old = store.graph().unwrap();
    let a = symbol_id(&old, "a");
    let assert_unchanged = || {
        assert_eq!(store.index_baseline().unwrap(), first);
        assert_eq!(store.status().unwrap().revision, first);
        assert_eq!(store.graph().unwrap(), old);
        let reopened = Store::open_for_tests(state.path(), work.path()).unwrap();
        assert_eq!(reopened.status().unwrap().revision, first);
        assert_eq!(reopened.graph().unwrap(), old);
        for document in &captured.1.revision.documents {
            let (stored, bytes) = reopened
                .native_source_at(first, &document.key)
                .unwrap()
                .unwrap();
            assert_eq!(stored, *document);
            assert_eq!(
                bytes,
                captured
                    .2
                    .files
                    .iter()
                    .find(|file| file.path == document.key.path)
                    .unwrap()
                    .text
                    .as_bytes()
                    .to_vec()
            );
        }
    };
    let mut duplicate = old.clone();
    duplicate.nodes.push(old.nodes[0].clone());
    assert!(
        store
            .publish_native(
                &duplicate,
                &captured.2,
                &captured.1,
                &leader,
                first,
                &cancel()
            )
            .is_err()
    );
    assert_unchanged();
    assert!(
        store
            .publish_native(
                &captured.0,
                &captured.2,
                &captured.1,
                &leader,
                baseline,
                &cancel()
            )
            .unwrap_err()
            .to_string()
            .starts_with("revision conflict")
    );
    assert_unchanged();
    assert!(
        store
            .publish_native(
                &captured.0,
                &captured.2,
                &captured.1,
                &leader,
                first,
                &Arc::new(AtomicBool::new(true))
            )
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    assert_unchanged();
    drop(store);
    let store = Store::open_for_tests(state.path(), work.path()).unwrap();
    assert_eq!(store.graph().unwrap(), old);
    assert_eq!(
        store.source("a.js").unwrap().unwrap().text,
        captured.0.files[0].text
    );
    for document in &captured.1.revision.documents {
        let (stored, bytes) = store
            .native_source_at(first, &document.key)
            .unwrap()
            .unwrap();
        assert_eq!(stored, *document);
        assert_eq!(
            bytes,
            captured
                .2
                .files
                .iter()
                .find(|file| file.path == document.key.path)
                .unwrap()
                .text
                .as_bytes()
                .to_vec()
        );
    }
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
            &leader,
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
    let leader = store.leader().unwrap();
    let first = publish_bundle(&store, &captured, store.index_baseline().unwrap(), &leader);
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
        db.query_row("SELECT count(*) FROM graph_nodes n JOIN revision_documents rd ON rd.graph_projection_id=n.projection_id", [], |r| r.get::<_, i64>(0))
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
    let leader = store.leader().unwrap();
    publish_bundle(&store, &captured, store.index_baseline().unwrap(), &leader);
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
    let leader = store.leader().unwrap();
    publish_bundle(&store, &captured, store.index_baseline().unwrap(), &leader);
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
    let leader = store.leader().unwrap();
    publish_bundle(&store, &captured, store.index_baseline().unwrap(), &leader);
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
    drop(leader);
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
    let saved = store.views().unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].view, view);
    assert_eq!(saved[0].orphaned_ids.len(), 3);
    assert_eq!(
        saved[0]
            .orphaned_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([a.clone(), b.clone(), "missing".into()])
    );
    let fresh = bundle(&store, &work);
    let leader = store.leader().unwrap();
    publish_bundle(&store, &fresh, store.index_baseline().unwrap(), &leader);
    let restored_note = store.annotations().unwrap().remove(0);
    assert!(restored_note.orphaned);
    assert_eq!(
        restored_note.attachment.availability,
        AttachmentAvailability::Anchorless
    );
    // Public graph-only writes cannot remove indexed symbols.
    let expected = store.status().unwrap().revision;
    assert!(
        store
            .publish(&Graph::default(), &leader, expected, &cancel())
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
    publish_bundle(&store, &removed, store.index_baseline().unwrap(), &leader);
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
fn legacy_admission_distinguishes_invalid_marker_from_live_decode() {
    let seed_records = |store: &Store| {
        store
            .put_annotation(&Annotation {
                id: "legacy-note".into(),
                node_id: "a".into(),
                body: "durable annotation".into(),
            })
            .unwrap();
        store
            .put_view(&SavedView {
                id: "legacy-view".into(),
                title: "Durable view".into(),
                query: query(),
                pins: BTreeMap::from([("missing".into(), Position { x: 1., y: 2. })]),
                hidden: vec!["b".into()],
            })
            .unwrap();
    };

    let (marker_state, marker_work, marker_store) = fixture();
    seed_records(&marker_store);
    let marker_path = index_db(marker_state.path());
    drop(marker_store);
    let marker_db = rusqlite::Connection::open(&marker_path).unwrap();
    downgrade_to_legacy(&marker_db);
    marker_db
        .execute(
            "UPDATE index_metadata SET extractor_version='wrong-old-extractor'",
            [],
        )
        .unwrap();
    drop(marker_db);
    let marker_store = Store::open_for_tests(marker_state.path(), marker_work.path()).unwrap();
    for error in [
        marker_store.annotations().unwrap_err(),
        marker_store.view("legacy-view").unwrap_err(),
        marker_store.views().unwrap_err(),
    ] {
        assert_eq!(
            error.to_string(),
            "incompatible_index: reconciliation required after invalid current index"
        );
    }

    let (stats_state, stats_work, stats_store) = fixture();
    seed_records(&stats_store);
    write_source(&stats_work);
    let captured = bundle(&stats_store, &stats_work);
    let session = stats_store.leader_session().unwrap();
    publish_bundle(
        &stats_store,
        &captured,
        stats_store.index_baseline().unwrap(),
        session.leader_guard().unwrap(),
    );
    assert_eq!(
        stats_store.annotations().unwrap()[0].annotation.id,
        "legacy-note"
    );
    assert_eq!(
        stats_store.view("legacy-view").unwrap().unwrap().view.id,
        "legacy-view"
    );
    let stats_clone = stats_store.clone();
    let stats_path = index_db(stats_state.path());
    let stats_db = rusqlite::Connection::open(&stats_path).unwrap();
    stats_db
        .execute("UPDATE index_metadata SET stats='not-json'", [])
        .unwrap();
    drop(stats_db);
    let first = stats_store.annotations().unwrap_err();
    assert!(
        first
            .to_string()
            .contains("incompatible_index: live index decode failed"),
        "{first:#}"
    );
    for closed in [
        stats_store.view("legacy-view").unwrap_err(),
        stats_clone.view("legacy-view").unwrap_err(),
        stats_clone.views().unwrap_err(),
    ] {
        assert_eq!(
            closed.to_string(),
            "incompatible_index: reconciliation required after invalid current index"
        );
    }
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
                match store.leader() {
                    Ok(leader) => {
                        let result = store.publish_native(
                            &captured.0,
                            &captured.2,
                            &captured.1,
                            &leader,
                            baseline,
                            &cancel(),
                        );
                        (result, Some(leader))
                    }
                    Err(error) => (Err(error), None),
                }
            })
        })
        .collect();
    barrier.wait();
    let outcomes: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(
        outcomes.iter().filter(|(result, _)| result.is_ok()).count(),
        1
    );
    assert!(
        outcomes
            .iter()
            .find_map(|(result, _)| result.as_ref().err())
            .unwrap()
            .to_string()
            .starts_with("revision conflict")
            || outcomes.iter().any(|(result, _)| result
                .as_ref()
                .err()
                .is_some_and(|e| e.to_string().starts_with("storage_busy")))
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|(result, leader)| result.is_ok() && leader.is_some())
            .count(),
        1
    );
    assert_eq!(store.status().unwrap().revision.index_revision, 1);
}

#[test]
fn durable_orphan_fallback_does_not_mask_current_index_corruption() {
    let (state, work, store) = fixture();
    write_source(&work);
    let captured = bundle(&store, &work);
    let leader = store.leader().unwrap();
    publish_bundle(&store, &captured, store.index_baseline().unwrap(), &leader);
    let saved: SavedView = serde_json::from_value(serde_json::json!({
        "id":"saved","title":"Saved","query":{"seed":"missing"}
    }))
    .unwrap();
    store.put_view(&saved).unwrap();
    let clone = store.clone();
    let db = rusqlite::Connection::open(index_db(state.path())).unwrap();
    db.execute(
        "UPDATE index_metadata SET reconcile_options='{}' WHERE singleton=1",
        [],
    )
    .unwrap();
    drop(db);
    for error in [store.views().unwrap_err(), clone.view("saved").unwrap_err()] {
        assert!(
            error.to_string().contains("incompatible_index"),
            "{error:#}"
        );
    }
}

#[test]
fn durable_storage_io_failure_is_never_converted_to_orphan_success() {
    let (state, _work, store) = fixture();
    let saved: SavedView = serde_json::from_value(serde_json::json!({
        "id":"durable-io","title":"Durable IO","query":{"seed":"missing"}
    }))
    .unwrap();
    store.put_view(&saved).unwrap();
    let record = state.path().join("data/workspaces").join(store.root_id());
    let database = record.join("workspace.db");
    let retained = record.join("workspace.db.retained");
    std::fs::rename(&database, &retained).unwrap();
    std::fs::create_dir(&database).unwrap();
    let error = store.views().unwrap_err();
    assert!(!error.to_string().contains("index_not_ready"), "{error:#}");
    std::fs::remove_dir(&database).unwrap();
    std::fs::rename(&retained, &database).unwrap();
}

#[test]
fn failed_leader_on_current_root_mismatch_never_enables_durable_orphans() {
    let (state, work, store) = fixture();
    write_source(&work);
    let captured = bundle(&store, &work);
    let leader = store.leader().unwrap();
    publish_bundle(&store, &captured, store.index_baseline().unwrap(), &leader);
    let saved: SavedView = serde_json::from_value(serde_json::json!({
        "id":"root-mismatch","title":"Root mismatch","query":{"seed":"missing"}
    }))
    .unwrap();
    let annotation = Annotation {
        id: "root-note".into(),
        node_id: "missing".into(),
        body: "keep".into(),
    };
    store.put_view(&saved).unwrap();
    store.put_annotation(&annotation).unwrap();
    drop(leader);
    let db = rusqlite::Connection::open(index_db(state.path())).unwrap();
    db.execute(
        "UPDATE index_metadata SET root_inode='0' WHERE singleton=1",
        [],
    )
    .unwrap();
    drop(db);
    let acquisition = store.leader_session().unwrap_err();
    assert!(
        acquisition.to_string().contains("root_changed"),
        "{acquisition:#}"
    );
    for error in [store.views().unwrap_err(), store.annotations().unwrap_err()] {
        assert!(error.to_string().contains("root_changed"), "{error:#}");
        assert!(!error.to_string().contains("index_not_ready"), "{error:#}");
    }
}

#[test]
fn durable_orphans_after_failed_takeover_but_not_current_marker_corruption() {
    let (_state, work, store) = fixture();
    write_source(&work);
    let captured = bundle(&store, &work);
    let leader = store.leader().unwrap();
    publish_bundle(&store, &captured, store.index_baseline().unwrap(), &leader);
    let saved: SavedView = serde_json::from_value(serde_json::json!({
        "id":"takeover","title":"Takeover","query":{"seed":"missing"}
    }))
    .unwrap();
    store.put_view(&saved).unwrap();
    drop(leader);
    let failed_takeover = store.leader_session().unwrap();
    let coordinator = baleyg::index_coordinator::IndexJobCoordinator::prepare_with_session(
        &store,
        None,
        failed_takeover.clone(),
    )
    .unwrap();
    let cancelled: CancelFlag = Arc::new(AtomicBool::new(true));
    let error = coordinator
        .run(
            &baleyg::indexer::IndexOptions::new(work.path().to_owned()),
            &cancelled,
            |_| {},
        )
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"), "{error:#}");
    drop(failed_takeover);
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    let orphan = store.view("takeover").unwrap().unwrap();
    assert_eq!(orphan.view, saved);
    assert_eq!(orphan.orphaned_ids, vec!["missing"]);

    let (state, work, store) = fixture();
    write_source(&work);
    let captured = bundle(&store, &work);
    let leader = store.leader().unwrap();
    publish_bundle(&store, &captured, store.index_baseline().unwrap(), &leader);
    store.put_view(&saved).unwrap();
    let db = rusqlite::Connection::open(index_db(state.path())).unwrap();
    db.execute(
        "UPDATE index_metadata SET reconciled_incarnation='not-a-uuid' WHERE singleton=1",
        [],
    )
    .unwrap();
    drop(db);
    let clone = store.clone();
    let first = store.status().unwrap_err();
    assert!(
        first.to_string().contains("incompatible_index"),
        "{first:#}"
    );
    for error in [
        store.views().unwrap_err(),
        clone.view("takeover").unwrap_err(),
    ] {
        assert!(
            error.to_string().contains("incompatible_index"),
            "{error:#}"
        );
    }
    drop(leader);
}

#[test]
fn null_current_marker_latches_before_durable_orphan_fallback() {
    let (state, work, store) = fixture();
    write_source(&work);
    let captured = bundle(&store, &work);
    let leader = store.leader().unwrap();
    publish_bundle(&store, &captured, store.index_baseline().unwrap(), &leader);
    let saved: SavedView = serde_json::from_value(serde_json::json!({
        "id":"null-marker","title":"Null marker","query":{"seed":"missing"}
    }))
    .unwrap();
    store.put_view(&saved).unwrap();
    let clone = store.clone();
    let db = rusqlite::Connection::open(index_db(state.path())).unwrap();
    db.execute(
        "UPDATE index_metadata SET reconciled_incarnation=NULL WHERE singleton=1",
        [],
    )
    .unwrap();
    drop(db);
    let first = store.status().unwrap_err();
    assert!(
        first.to_string().contains("incompatible_index"),
        "{first:#}"
    );
    let latched = clone.views().unwrap_err();
    assert!(
        latched.to_string().contains("incompatible_index"),
        "{latched:#}"
    );
    drop(leader);
}

#[test]
fn recognized_noncurrent_without_marker_column_is_explicitly_not_ready() {
    let (state, work, store) = fixture();
    let saved: SavedView = serde_json::from_value(serde_json::json!({
        "id":"legacy","title":"Legacy","query":{"seed":"missing"}
    }))
    .unwrap();
    store.put_view(&saved).unwrap();
    let before = std::fs::read(index_db(state.path())).unwrap();
    let leader = store.leader().unwrap();
    let error = store.status().unwrap_err();
    assert!(error.to_string().contains("index_not_ready"), "{error:#}");
    assert!(!error.to_string().contains("no such column"), "{error:#}");
    let orphan = store.view("legacy").unwrap().unwrap();
    assert_eq!(orphan.view, saved);
    assert_eq!(orphan.orphaned_ids, vec!["missing"]);
    assert_eq!(std::fs::read(index_db(state.path())).unwrap(), before);
    drop(leader);
    drop(work);
}

#[test]
fn malformed_graph_rolls_back_and_structural_stats_are_recounted() {
    let (state, work, store) = fixture();
    write_source(&work);
    let captured = bundle(&store, &work);
    let leader = store.leader().unwrap();
    let first = publish_bundle(&store, &captured, store.index_baseline().unwrap(), &leader);
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
                .publish_native(&bad, &captured.2, &captured.1, &leader, first, &cancel())
                .is_err(),
            "case {kind}"
        );
        assert_eq!(store.index_baseline().unwrap(), first);
        assert_eq!(store.status().unwrap().revision, first);
        assert_eq!(store.graph().unwrap(), baseline);
        let reopened = Store::open_for_tests(state.path(), work.path()).unwrap();
        assert_eq!(reopened.status().unwrap().revision, first);
        assert_eq!(reopened.graph().unwrap(), baseline);
        for document in &captured.1.revision.documents {
            let (stored, bytes) = reopened
                .native_source_at(first, &document.key)
                .unwrap()
                .unwrap();
            assert_eq!(stored, *document);
            assert_eq!(
                bytes,
                captured
                    .2
                    .files
                    .iter()
                    .find(|file| file.path == document.key.path)
                    .unwrap()
                    .text
                    .as_bytes()
                    .to_vec()
            );
        }
    }
    let next = publish_bundle(&store, &captured, store.index_baseline().unwrap(), &leader);
    assert_eq!(store.status().unwrap().revision, next);
    assert_eq!(store.graph().unwrap(), baseline);
}

#[test]
fn cache_loss_never_reuses_revision_tokens_and_sql_enforces_foreign_keys() {
    let (state, work, store) = fixture();
    write_source(&work);
    let captured = bundle(&store, &work);
    let leader = store.leader().unwrap();
    let rev = publish_bundle(&store, &captured, store.index_baseline().unwrap(), &leader);
    let db = rusqlite::Connection::open(index_db(state.path())).unwrap();
    db.pragma_update(None, "foreign_keys", true).unwrap();
    assert!(
        db.execute("DELETE FROM document_versions WHERE id=(SELECT document_version_id FROM revision_documents WHERE path='a.js')", [])
            .is_err()
    );
    let a = symbol_id(&captured.0, "a");
    assert!(db.execute("DELETE FROM graph_nodes WHERE projection_id=(SELECT graph_projection_id FROM revision_documents WHERE path='a.js') AND id=?1", [&a]).is_err());
    drop(db);
    drop(leader);
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
    let leader = store.leader().unwrap();
    assert!(
        store
            .publish_native(&fresh.0, &fresh.2, &fresh.1, &leader, rev, &cancel())
            .is_err()
    );
    let new_rev = publish_bundle(&store, &fresh, store.index_baseline().unwrap(), &leader);
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
    let leader = store.leader().unwrap();
    let previous = publish_bundle(&store, &captured, store.index_baseline().unwrap(), &leader);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let path = index_db(state.path());
    let writer_path = path.clone();
    let writer = std::thread::spawn(move || {
        let db = rusqlite::Connection::open(writer_path).unwrap();
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

    // Capture every active-journal outcome before releasing the writer, so no
    // assertion can strand the writer thread or its leader guard.
    let journal = path.with_file_name("index.db-journal");
    let active_journal_exists = journal.exists();
    let active_status = store.status();
    let active_source = store.source_at("a.js", Some(previous));
    let raw_pair = (|| -> rusqlite::Result<(String, i64, String, String, Vec<u8>, String)> {
        let db = rusqlite::Connection::open(&path)?;
        db.busy_timeout(std::time::Duration::ZERO)?;
        db.query_row(
            "SELECT m.index_generation,m.index_revision,d.path,d.content_hash,d.source_bytes,d.content_hash FROM index_metadata m JOIN native_revisions r ON r.published_index_revision=m.index_revision JOIN revision_documents rd ON rd.revision_id=r.id JOIN document_versions d ON d.id=rd.document_version_id WHERE m.singleton=1 AND rd.path='a.js'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
    })();
    done_tx.send(()).unwrap();
    writer.join().unwrap();

    assert!(active_journal_exists);
    match active_status {
        Ok(status) => assert_eq!(status.revision, previous),
        Err(error) => {
            let text = error.to_string();
            assert!(
                text.contains("index_not_ready") || text.contains("storage_busy"),
                "{error:#}"
            );
        }
    }
    match active_source {
        Ok(Some((pin, source))) => {
            assert_eq!(pin, previous);
            assert_eq!(source.text, captured.0.files[0].text);
            assert_eq!(source.hash, captured.0.files[0].hash);
            assert_eq!(source.text.as_bytes(), captured.2.files[0].text.as_bytes());
        }
        Ok(None) => panic!("active DELETE journal hid the prior committed source"),
        Err(error) => {
            let text = error.to_string();
            assert!(
                text.contains("index_not_ready") || text.contains("storage_busy"),
                "{error:#}"
            );
        }
    }
    match raw_pair {
        Ok((generation, revision, path, file_hash, native_bytes, native_hash)) => {
            assert_eq!(generation, previous.index_generation.to_string());
            assert_eq!(revision, previous.index_revision as i64);
            assert_eq!(path, "a.js");
            assert_eq!(file_hash, captured.0.files[0].hash);
            assert_eq!(native_hash, captured.0.files[0].hash);
            assert_eq!(native_bytes, captured.0.files[0].text.as_bytes().to_vec());
        }
        Err(error) => assert!(
            matches!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
            ),
            "{error}"
        ),
    }
    assert_eq!(store.index_baseline().unwrap(), previous);
    for error in [
        store.status().unwrap_err(),
        store.source_at("a.js", Some(previous)).unwrap_err(),
    ] {
        assert!(error.to_string().contains("index_not_ready"), "{error:#}");
    }

    std::fs::write(&journal, [0u8; 512]).unwrap();
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
    let leader = store.leader().unwrap();
    let next = store
        .publish_native(&graph, &capture, &native, &leader, first, &cancel())
        .unwrap();
    assert_ne!(next.index_generation, first.index_generation);
    assert_eq!(next.index_revision, 1);
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
            .publish_native(&graph, &capture, &native, &leader, first, &cancel())
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
    let leader = store.leader().unwrap();
    let before = publish_bundle(&store, &captured, store.index_baseline().unwrap(), &leader);
    let clone = store.clone();
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
    drop(db);
    drop(leader);
    assert_eq!(store.index_baseline().unwrap(), before);
    for error in [
        store.status().unwrap_err(),
        store.source("a.js").unwrap_err(),
        store.graph().unwrap_err(),
        clone.status().unwrap_err(),
        clone.source("a.js").unwrap_err(),
        clone.graph().unwrap_err(),
    ] {
        assert!(
            error.to_string().contains("incompatible_index"),
            "{error:#}"
        );
    }
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
fn removed_or_renamed_anchor_documents_are_missing_without_poisoning_saved_lists() {
    for rename in [false, true] {
        let (_state, work, store) = fixture();
        std::fs::write(
            work.path().join("a.js"),
            "function stable() { return 1; }\n",
        )
        .unwrap();
        std::fs::write(
            work.path().join("b.js"),
            "function target() { return 2; }\n",
        )
        .unwrap();
        let captured = bundle(&store, &work);
        let leader = store.leader().unwrap();
        let stable = captured
            .1
            .declarations
            .iter()
            .find(|row| row.document.path == "a.js" && row.name.as_deref() == Some("stable"))
            .unwrap()
            .syntax_id
            .clone();
        let target = captured
            .1
            .declarations
            .iter()
            .find(|row| row.document.path == "b.js" && row.name.as_deref() == Some("target"))
            .unwrap()
            .syntax_id
            .clone();
        let first = publish_bundle(&store, &captured, store.index_baseline().unwrap(), &leader);
        let saved_view = |id: &str, title: &str, seed: &str| SavedView {
            id: id.into(),
            title: title.into(),
            query: ViewQuery {
                seed: seed.into(),
                ..query()
            },
            pins: BTreeMap::new(),
            hidden: vec![],
        };
        let missing_view = store
            .save_view_at(first, &saved_view("target-view", "Target", &target))
            .unwrap();
        let stable_view = store
            .save_view_at(first, &saved_view("stable-view", "Stable", &stable))
            .unwrap();
        let missing_note = store
            .save_annotation_at(
                first,
                &AnnotationRequest {
                    id: "target-note".into(),
                    node_id: target.clone(),
                    body: "target note".into(),
                    title: None,
                },
            )
            .unwrap();
        let stable_note = store
            .save_annotation_at(
                first,
                &AnnotationRequest {
                    id: "stable-note".into(),
                    node_id: stable,
                    body: "stable note".into(),
                    title: None,
                },
            )
            .unwrap();
        let missing_view_anchor = missing_view.view.anchor.unwrap().get().to_owned();
        let stable_view_anchor = stable_view.view.anchor.unwrap().get().to_owned();
        let missing_note_anchor = missing_note.annotation.anchor.unwrap().get().to_owned();
        let stable_note_anchor = stable_note.annotation.anchor.unwrap().get().to_owned();

        if rename {
            std::fs::rename(work.path().join("b.js"), work.path().join("renamed.js")).unwrap();
        } else {
            std::fs::remove_file(work.path().join("b.js")).unwrap();
        }
        let fresh = bundle(&store, &work);
        let second = publish_bundle(&store, &fresh, first, &leader);

        let views = store.saved_views_at(Some(second)).unwrap();
        assert_eq!(views.len(), 2, "rename={rename}");
        let missing = views
            .iter()
            .find(|state| state.view.id == "target-view")
            .unwrap();
        assert_eq!(
            missing.attachment.availability,
            AttachmentAvailability::Ready,
            "rename={rename}"
        );
        assert_eq!(
            missing.attachment.result.as_ref().unwrap().status,
            AnchorStatus::Orphaned,
            "rename={rename}"
        );
        assert_eq!(
            missing.attachment.result.as_ref().unwrap().reason,
            AnchorReason::Missing,
            "rename={rename}"
        );
        assert_eq!(
            missing.view.anchor.as_deref().unwrap().get(),
            missing_view_anchor,
            "rename={rename}"
        );
        let unaffected = views
            .iter()
            .find(|state| state.view.id == "stable-view")
            .unwrap();
        assert_eq!(
            unaffected.attachment.result.as_ref().unwrap().status,
            AnchorStatus::Attached,
            "rename={rename}"
        );
        assert_eq!(
            unaffected.view.anchor.as_deref().unwrap().get(),
            stable_view_anchor,
            "rename={rename}"
        );
        let opened = store
            .saved_view_at("target-view", Some(second))
            .unwrap()
            .unwrap();
        assert_eq!(
            opened.attachment.result.as_ref().unwrap().reason,
            AnchorReason::Missing,
            "rename={rename}"
        );
        assert_eq!(
            opened.view.anchor.as_deref().unwrap().get(),
            missing_view_anchor,
            "rename={rename}"
        );

        let notes = store.saved_annotations_at(Some(second)).unwrap();
        assert_eq!(notes.len(), 2, "rename={rename}");
        let missing = notes
            .iter()
            .find(|state| state.annotation.id == "target-note")
            .unwrap();
        assert_eq!(
            missing.attachment.availability,
            AttachmentAvailability::Ready,
            "rename={rename}"
        );
        assert_eq!(
            missing.attachment.result.as_ref().unwrap().status,
            AnchorStatus::Orphaned,
            "rename={rename}"
        );
        assert_eq!(
            missing.attachment.result.as_ref().unwrap().reason,
            AnchorReason::Missing,
            "rename={rename}"
        );
        assert_eq!(
            missing.annotation.anchor.as_deref().unwrap().get(),
            missing_note_anchor,
            "rename={rename}"
        );
        let unaffected = notes
            .iter()
            .find(|state| state.annotation.id == "stable-note")
            .unwrap();
        assert_eq!(
            unaffected.attachment.result.as_ref().unwrap().status,
            AnchorStatus::Attached,
            "rename={rename}"
        );
        assert_eq!(
            unaffected.annotation.anchor.as_deref().unwrap().get(),
            stable_note_anchor,
            "rename={rename}"
        );
    }
}

#[test]
fn same_path_different_association_is_missing_but_dangling_revision_fails_closed() {
    let (state, work, store) = fixture();
    std::fs::write(
        work.path().join("b.js"),
        "function target() { return 2; }\n",
    )
    .unwrap();
    let captured = bundle(&store, &work);
    let leader = store.leader().unwrap();
    let target = captured
        .1
        .declarations
        .iter()
        .find(|row| row.document.path == "b.js" && row.name.as_deref() == Some("target"))
        .unwrap()
        .syntax_id
        .clone();
    let pin = publish_bundle(&store, &captured, store.index_baseline().unwrap(), &leader);
    store
        .save_view_at(
            pin,
            &SavedView {
                id: "association".into(),
                title: "Association".into(),
                query: ViewQuery {
                    seed: target,
                    ..query()
                },
                pins: BTreeMap::new(),
                hidden: vec![],
            },
        )
        .unwrap();

    let identity =
        baleyg::store::topology::WorkspaceIdentity::discover(Some(work.path()), work.path())
            .unwrap();
    let roots = baleyg::store::topology::TopologyRoots::isolated_for_tests(
        state.path().join("cache"),
        state.path().join("data"),
    );
    let records = rusqlite::Connection::open(roots.record_db(&identity)).unwrap();
    let original_payload: String = records
        .query_row(
            "SELECT payload FROM views WHERE id='association'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut changed_association: serde_json::Value =
        serde_json::from_str(&original_payload).unwrap();
    changed_association["anchor"]["document"]["sourceSetId"] =
        serde_json::json!("source-set:v1:changed");
    let changed_anchor = serde_json::value::to_raw_value(&changed_association["anchor"])
        .unwrap()
        .get()
        .to_owned();
    records
        .execute(
            "UPDATE views SET payload=?1 WHERE id='association'",
            [serde_json::to_string(&changed_association).unwrap()],
        )
        .unwrap();

    let listed = store.saved_views_at(Some(pin)).unwrap();
    let listed = listed
        .iter()
        .find(|state| state.view.id == "association")
        .unwrap();
    assert_eq!(
        listed.attachment.availability,
        AttachmentAvailability::Ready
    );
    assert_eq!(
        listed.attachment.result.as_ref().unwrap().status,
        AnchorStatus::Orphaned
    );
    assert_eq!(
        listed.attachment.result.as_ref().unwrap().reason,
        AnchorReason::Missing
    );
    assert_eq!(listed.view.anchor.as_deref().unwrap().get(), changed_anchor);
    let opened = store
        .saved_view_at("association", Some(pin))
        .unwrap()
        .unwrap();
    assert_eq!(
        opened.attachment.result.as_ref().unwrap().reason,
        AnchorReason::Missing
    );
    assert_eq!(opened.view.anchor.as_deref().unwrap().get(), changed_anchor);

    records
        .execute(
            "UPDATE views SET payload=?1 WHERE id='association'",
            [&original_payload],
        )
        .unwrap();
    drop(records);
    let cache = rusqlite::Connection::open(index_db(state.path())).unwrap();
    cache.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    cache
        .execute(
            "UPDATE revision_documents SET document_version_id='document:v1:dangling' WHERE path='b.js'",
            [],
        )
        .unwrap();
    cache.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    drop(cache);

    let first = store.saved_views_at(Some(pin)).unwrap_err();
    let first_detail = format!("{first:#}");
    assert!(
        first_detail == "Query returned no rows"
            || first_detail
                == "incompatible_index: reconciliation required after invalid current index"
            || first_detail.starts_with(
                "incompatible_index: selected evidence decode failed: Query returned no rows",
            ),
        "{first_detail}"
    );
    let second = store.saved_view_at("association", Some(pin)).unwrap_err();
    assert_eq!(
        second.to_string(),
        "incompatible_index: reconciliation required after invalid current index",
        "{second:#}"
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

#[test]
fn malformed_persisted_anchors_fail_closed_without_an_index() {
    let (state, work, store) = fixture();
    let view = SavedView {
        id: "view-malformed".into(),
        title: "View".into(),
        query: query(),
        pins: BTreeMap::new(),
        hidden: vec![],
    };
    let note = Annotation {
        id: "note-malformed".into(),
        node_id: "node".into(),
        body: "body".into(),
    };
    store.put_view(&view).unwrap();
    store.put_annotation(&note).unwrap();
    let identity =
        baleyg::store::topology::WorkspaceIdentity::discover(Some(work.path()), work.path())
            .unwrap();
    let roots = baleyg::store::topology::TopologyRoots::isolated_for_tests(
        state.path().join("cache"),
        state.path().join("data"),
    );
    let db = rusqlite::Connection::open(roots.record_db(&identity)).unwrap();
    let original_view: String = db
        .query_row("SELECT payload FROM views WHERE id=?1", [&view.id], |row| {
            row.get(0)
        })
        .unwrap();
    let original_note: String = db
        .query_row(
            "SELECT payload FROM annotations WHERE id=?1",
            [&note.id],
            |row| row.get(0),
        )
        .unwrap();
    let malformed_anchor = serde_json::json!({
        "syntaxId":"sid:v1:0123456789abcdef0123456789abcdef",
        "document":{"sourceSetId":"set","language":"typescript","path":"../escape.rs"},
        "capturedRevisionId":"revision",
        "headerHash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "siblingGroupHash":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "siblingCount":1,"identicalHeaderCount":1
    });
    let mut malformed_view: serde_json::Value = serde_json::from_str(&original_view).unwrap();
    malformed_view["anchor"] = malformed_anchor.clone();
    db.execute(
        "UPDATE views SET payload=?1 WHERE id=?2",
        rusqlite::params![serde_json::to_string(&malformed_view).unwrap(), view.id],
    )
    .unwrap();
    assert!(
        store
            .views()
            .unwrap_err()
            .to_string()
            .contains("invalid durable anchor")
    );
    assert!(
        store
            .view("view-malformed")
            .unwrap_err()
            .to_string()
            .contains("invalid durable anchor")
    );
    db.execute(
        "UPDATE views SET payload=?1 WHERE id=?2",
        rusqlite::params![original_view, view.id],
    )
    .unwrap();

    let mut malformed_note: serde_json::Value = serde_json::from_str(&original_note).unwrap();
    malformed_note["anchor"] = malformed_anchor;
    db.execute(
        "UPDATE annotations SET payload=?1 WHERE id=?2",
        rusqlite::params![serde_json::to_string(&malformed_note).unwrap(), note.id],
    )
    .unwrap();
    assert!(
        store
            .annotations()
            .unwrap_err()
            .to_string()
            .contains("invalid durable anchor")
    );
}
