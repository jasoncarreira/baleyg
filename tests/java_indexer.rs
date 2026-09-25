mod common;
use baleyg::{
    indexer::{IndexOptions, index_workspace},
    model::*,
};
use std::{
    collections::BTreeSet,
    fs,
    sync::{Arc, atomic::AtomicBool},
};
fn run(o: &IndexOptions) -> Graph {
    index_workspace(o, &Arc::new(AtomicBool::new(false)), |_| {}).unwrap()
}
#[test]
fn lexical_owners_constructors_lambdas_anonymous_classes_and_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let source = r#"package demo;
@interface Mark { String value() default "not invocation"; }
interface Work { void signature(); default void fallback() { backup(); } }
record Pair(String name) { Pair { validate(name); } }
enum Mode { ON { void act() { enumOnly(); } }; }
@Mark("héllo")
class Café {
    Café() { this(argument()); }
    Café(Object value) { super(); }
    native void nativeOnly();
    void run() {
        use((x) -> lambdaOnly(x));
        Runnable r = () -> { nestedOnly(); };
        Object a = new Base(constructorArg()) {
            { initializerOnly(); }
            void inside() { anonymousOnly(); }
        };
        class Local { void local() { localOnly(); } }
        if (check()) { receiver().next(inner()); }
        after();
    }
}
"#;
    fs::write(dir.path().join("Café.java"), source).unwrap();
    fs::write(dir.path().join("script.js"), "function js() { jsOnly(); }").unwrap();
    fs::write(dir.path().join("lib.rs"), "fn rust() { rust_only(); }").unwrap();
    let opts = IndexOptions::new(dir.path().to_owned());
    let g = run(&opts);
    assert_eq!(g, run(&opts));
    assert_eq!(g.stats.parse_error_files, 0, "{:?}", g.diagnostics);
    assert_eq!(g.files.len(), 3);
    let class = g.nodes.iter().find(|n| n.name == "Café").unwrap();
    assert_eq!(class.kind, SymbolKind::Class);
    let method = g.nodes.iter().find(|n| n.name == "run").unwrap();
    assert_eq!(method.parent.as_ref(), Some(&class.id));
    let constructors: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.name == "Café" && n.kind == SymbolKind::Method)
        .collect();
    assert_eq!(constructors.len(), 2);
    assert!(
        constructors
            .iter()
            .all(|c| c.parent.as_ref() == Some(&class.id))
    );
    let call = |name: &str| g.calls.iter().find(|c| c.callee_text == name).unwrap();
    assert_eq!(call("after").caller, method.id);
    assert_eq!(call("constructorArg").caller, method.id);
    assert_eq!(call("new Base").caller, method.id);
    for name in [
        "lambdaOnly",
        "nestedOnly",
        "anonymousOnly",
        "initializerOnly",
        "localOnly",
        "enumOnly",
    ] {
        assert_ne!(call(name).caller, method.id, "{name}");
    }
    let lambda = g
        .nodes
        .iter()
        .find(|n| n.id == call("lambdaOnly").caller)
        .unwrap();
    assert_eq!(lambda.parent.as_ref(), Some(&method.id));
    assert_eq!(
        call("use").callback_arguments.as_slice(),
        std::slice::from_ref(&lambda.id)
    );
    let anon_method = g.nodes.iter().find(|n| n.name == "inside").unwrap();
    let anon_class = g
        .nodes
        .iter()
        .find(|n| Some(&n.id) == anon_method.parent.as_ref())
        .unwrap();
    assert!(anon_class.name.starts_with("<anonymous@"));
    assert_eq!(anon_class.kind, SymbolKind::Class);
    assert_eq!(call("initializerOnly").caller, anon_class.id);
    assert_eq!(anon_class.parent.as_ref(), Some(&method.id));
    let initializer = g
        .nodes
        .iter()
        .find(|n| n.id == call("validate").caller)
        .unwrap();
    assert_eq!(initializer.name, "Pair");
    assert_eq!(initializer.kind, SymbolKind::Method);
    assert!(call("receiver().next").regions.len() == 1);
    assert!(call("lambdaOnly").regions.is_empty());
    for c in g.calls.iter().filter(|c| c.path.ends_with(".java")) {
        assert_eq!(c.resolution, Resolution::Unresolved);
        assert!(c.target.is_none() && c.candidate_symbols.is_empty());
        assert_eq!(c.provenance.semantic, SemanticState::Unavailable);
        assert!(
            source.is_char_boundary(c.range.start_byte)
                && source.is_char_boundary(c.range.end_byte)
        );
        assert_eq!(
            c.range.start_line,
            source[..c.range.start_byte]
                .bytes()
                .filter(|b| *b == b'\n')
                .count()
                + 1
        );
        let line_start = source[..c.range.start_byte]
            .rfind('\n')
            .map_or(0, |i| i + 1);
        assert_eq!(c.range.start_column, c.range.start_byte - line_start + 1);
    }
}
#[test]
fn shared_start_call_ids_are_unique_and_publishable() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Calls.java"),
        "class Calls { void run() { receiver().next(inner(first()), second()).finish(); } }",
    )
    .unwrap();
    let graph = run(&IndexOptions::new(dir.path().to_owned()));
    assert_eq!(
        graph
            .calls
            .iter()
            .map(|c| &c.id)
            .collect::<BTreeSet<_>>()
            .len(),
        graph.calls.len()
    );
    assert!(graph.calls.iter().any(|a| {
        graph.calls.iter().any(|b| {
            a.range.start_byte == b.range.start_byte && a.range.end_byte != b.range.end_byte
        })
    }));
    let state = tempfile::tempdir().unwrap();
    let store = crate::common::open_store(&state.path().join("state"), dir.path()).unwrap();
    store
        .publish(
            &graph,
            &store.leader().unwrap(),
            store.status().unwrap().revision,
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
}
#[test]
fn recovery_and_cancellation_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Broken.java"),
        "class Broken { void run() { call( ; } }",
    )
    .unwrap();
    let options = IndexOptions::new(dir.path().to_owned());
    let graph = run(&options);
    assert_eq!(graph.stats.parse_error_files, 1);
    assert!(graph.diagnostics.iter().any(|d| d.code == "parse-error"));
    assert!(
        index_workspace(&options, &Arc::new(AtomicBool::new(true)), |_| {})
            .unwrap_err()
            .to_string()
            .contains("cancel")
    );
}

#[test]
fn stable_java_declarations_overloads_and_revision_local_occurrences() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Types.java");
    fs::write(&path, "class Types { void work(String s) { obj.foo(); } void work(int i) { obj.foo(); } void same() {} void same() {} }").unwrap();
    let first = run(&IndexOptions::new(dir.path().to_owned()));
    let methods: Vec<_> = first.nodes.iter().filter(|n| n.name == "work").collect();
    assert_eq!(methods.len(), 2);
    assert!(methods.iter().all(|n| n.id.starts_with("sid:v1:")));
    assert_ne!(methods[0].id, methods[1].id);
    let same: Vec<_> = first.nodes.iter().filter(|n| n.name == "same").collect();
    assert_eq!(same.len(), 2);
    assert_ne!(same[0].id, same[1].id);
    assert!(
        first
            .calls
            .iter()
            .all(|c| c.id.starts_with("occ:v1:") && c.target.is_none())
    );
    fs::write(&path, "class Types { void work(String s) { obj.foo(); extra(); } void work(int i) { obj.foo(); } void same() {} void same() {} }").unwrap();
    let second = run(&IndexOptions::new(dir.path().to_owned()));
    let second_methods: Vec<_> = second.nodes.iter().filter(|n| n.name == "work").collect();
    assert_eq!(
        methods.iter().map(|n| &n.id).collect::<Vec<_>>(),
        second_methods.iter().map(|n| &n.id).collect::<Vec<_>>()
    );
    assert_ne!(first.calls[0].id, second.calls[0].id);
    assert_eq!(
        same.iter().map(|n| &n.id).collect::<Vec<_>>(),
        second
            .nodes
            .iter()
            .filter(|n| n.name == "same")
            .map(|n| &n.id)
            .collect::<Vec<_>>()
    );
}
