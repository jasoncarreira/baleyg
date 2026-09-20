use baleyg::{
    indexer::{IndexOptions, index_workspace},
    model::*,
    store::Store,
};
use std::{
    fs,
    sync::{Arc, atomic::AtomicBool},
};
use tempfile::TempDir;
#[test]
fn real_syntax_rename_orphans_notes_and_cached_graph_preserves_call_order() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    let state = temp.path().join("state");
    fs::create_dir(&root).unwrap();
    let text = "function first() { a(); b(); c(); d(); e(); f(); g(); h(); i(); j(); k(); l(); m(); n(); o(); p(); q(); r(); s(); t(); u(); v(); w(); x(); y(); z(); }\n";
    fs::write(root.join("flow.js"), text).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let opts = IndexOptions::new(root.canonicalize().unwrap());
    let graph = index_workspace(&opts, &cancel, |_| {}).unwrap();
    let again = index_workspace(&opts, &cancel, |_| {}).unwrap();
    assert_eq!(graph, again);
    let symbol = graph.nodes.iter().find(|n| n.name == "first").unwrap();
    let old_id = symbol.id.clone();
    let store = Store::open(&state, &root).unwrap();
    let rev = store.publish(&graph, Some(0), &cancel).unwrap();
    assert_eq!(store.graph().unwrap(), graph);
    store
        .put_annotation(&Annotation {
            id: "note".into(),
            node_id: old_id.clone(),
            body: "belongs to first".into(),
        })
        .unwrap();
    let query = ViewQuery {
        seed: old_id.clone(),
        depth: 1,
        max_nodes: 40,
        max_calls: 3,
        include_callbacks: false,
        exclude_paths: vec![],
    };
    let view = store.query_view(&query).unwrap().unwrap();
    assert!(view.truncated);
    assert_eq!(
        view.calls
            .iter()
            .map(|c| c.callee_text.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "c"]
    );
    fs::write(root.join("flow.js"), text.replace("first", "other")).unwrap();
    let changed = index_workspace(&opts, &cancel, |_| {}).unwrap();
    assert!(changed.nodes.iter().all(|n| n.id != old_id));
    store.publish(&changed, Some(rev), &cancel).unwrap();
    assert!(store.annotations().unwrap()[0].orphaned);
    assert_eq!(
        store.annotations().unwrap()[0].annotation.body,
        "belongs to first"
    );
}

#[test]
fn class_instance_initializers_are_not_calls_from_definition_context() {
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("fields.js"),"function make() { if (ok) return class C { [fieldKey()] = build(); static eager = now(); [key()]() { body(); } }; }\n").unwrap();
    let graph = index_workspace(
        &IndexOptions::new(temp.path().canonicalize().unwrap()),
        &Arc::new(AtomicBool::new(false)),
        |_| {},
    )
    .unwrap();
    let owner = |callee: &str| {
        let call = graph
            .calls
            .iter()
            .find(|c| c.callee_text == callee)
            .unwrap();
        graph
            .nodes
            .iter()
            .find(|n| n.id == call.caller)
            .unwrap()
            .name
            .clone()
    };
    assert_eq!(owner("fieldKey"), "make");
    assert_eq!(owner("now"), "make");
    assert_eq!(owner("key"), "make");
    assert_eq!(owner("build"), "C");
    let build = graph
        .calls
        .iter()
        .find(|c| c.callee_text == "build")
        .unwrap();
    let regions: Vec<_> = graph
        .regions
        .iter()
        .filter(|r| build.regions.contains(&r.id))
        .collect();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].kind, "instance-initializer");
    assert!(
        graph
            .diagnostics
            .iter()
            .any(|d| d.code == "instance-initializer-boundary")
    );
}
#[test]
fn injected_sql_failure_after_insert_preserves_previous_revision() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    let state = temp.path().join("state");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("flow.js"), "function before() { a(); b(); }\n").unwrap();
    let options = IndexOptions::new(root.canonicalize().unwrap());
    let cancel = Arc::new(AtomicBool::new(false));
    let first = index_workspace(&options, &cancel, |_| {}).unwrap();
    let store = Store::open(&state, &root).unwrap();
    let revision = store.publish(&first, Some(0), &cancel).unwrap();
    fs::write(root.join("flow.js"), "function after() { c(); d(); }\n").unwrap();
    let next = index_workspace(&options, &cancel, |_| {}).unwrap();
    let id = next.calls[1].id.replace('\'', "''");
    let db = rusqlite::Connection::open(state.join("cache.db")).unwrap();
    db.execute_batch(&format!("CREATE TRIGGER abort_second_call BEFORE INSERT ON calls WHEN NEW.id='{id}' BEGIN SELECT RAISE(ABORT,'injected post-write failure'); END;")).unwrap();
    drop(db);
    let failure = store.publish(&next, Some(revision), &cancel).unwrap_err();
    assert!(failure.to_string().contains("injected post-write failure"));
    assert_eq!(store.status().unwrap().revision, revision);
    assert_eq!(store.graph().unwrap(), first);
    let db = rusqlite::Connection::open(state.join("cache.db")).unwrap();
    db.execute_batch("DROP TRIGGER abort_second_call").unwrap();
    drop(db);
    let next_revision = store.publish(&next, Some(revision), &cancel).unwrap();
    assert!(next_revision > revision + 1);
}
