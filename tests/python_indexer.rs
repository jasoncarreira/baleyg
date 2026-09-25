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
fn run(options: &IndexOptions) -> Graph {
    index_workspace(options, &Arc::new(AtomicBool::new(false)), |_| {}).unwrap()
}
#[test]
fn scopes_defaults_decorators_async_lambdas_utf8_and_publication() {
    let dir = tempfile::tempdir().unwrap();
    let source = "@decorate(factory())\nclass Café(base()):\n    class_value = initialize()\n    @method_decorator()\n    async def run(self, arg=default()):\n        def nested(x=nested_default()):\n            nested_body()\n        use(lambda x=lambda_default(): lambda_body(x))\n        receiver().write(inner(first()), second())\n        await request()\n        if check():\n            return finish()\n";
    fs::write(dir.path().join("sample.py"), source).unwrap();
    fs::write(dir.path().join("other.js"), "function js() { invoke(); }").unwrap();
    let options = IndexOptions::new(dir.path().to_owned());
    let graph = run(&options);
    assert_eq!(graph, run(&options));
    assert_eq!(graph.stats.parse_error_files, 0);
    assert_eq!(graph.stats.files, 2);
    let owner = |callee: &str| {
        graph
            .nodes
            .iter()
            .find(|n| {
                n.id == graph
                    .calls
                    .iter()
                    .find(|c| c.callee_text == callee)
                    .unwrap()
                    .caller
            })
            .unwrap()
    };
    assert_eq!(owner("factory").kind, SymbolKind::Module);
    assert_eq!(owner("base").kind, SymbolKind::Module);
    assert_eq!(owner("initialize").name, "Café");
    assert_eq!(owner("method_decorator").name, "Café");
    assert_eq!(owner("default").name, "Café");
    assert_eq!(owner("nested_default").name, "run");
    assert_eq!(owner("nested_body").name, "nested");
    assert_eq!(owner("nested_body").kind, SymbolKind::Function);
    assert_eq!(owner("lambda_default").name, "run");
    assert!(owner("lambda_body").name.starts_with("<lambda@"));
    assert_eq!(owner("request").kind, SymbolKind::Method);
    let callback = graph.calls.iter().find(|c| c.callee_text == "use").unwrap();
    assert_eq!(
        callback.callback_arguments,
        [owner("lambda_body").id.clone()]
    );
    let class = graph.nodes.iter().find(|n| n.name == "Café").unwrap();
    assert!(source[class.range.start_byte..class.range.end_byte].starts_with("class Café"));
    let ids: BTreeSet<_> = graph.calls.iter().map(|c| &c.id).collect();
    assert_eq!(ids.len(), graph.calls.len());
    assert!(graph.calls.iter().any(|a| {
        graph.calls.iter().any(|b| {
            a.range.start_byte == b.range.start_byte && a.range.end_byte != b.range.end_byte
        })
    }));
    for call in graph.calls.iter().filter(|c| c.path == "sample.py") {
        assert_eq!(call.provenance.semantic, SemanticState::Unavailable);
        assert_eq!(call.resolution, Resolution::Unresolved);
        assert!(call.target.is_none() && call.candidate_symbols.is_empty());
        assert!(
            source.is_char_boundary(call.range.start_byte)
                && source.is_char_boundary(call.range.end_byte)
        );
        let prefix = &source[..call.range.start_byte];
        assert_eq!(
            call.range.start_line,
            prefix.bytes().filter(|b| *b == b'\n').count() + 1
        );
        assert_eq!(
            call.range.start_column,
            prefix.rsplit('\n').next().unwrap().len() + 1
        );
    }
    assert!(!graph.regions.is_empty());
    let state = tempfile::tempdir().unwrap();
    let store = crate::common::open_store(&state.path().join("state"), dir.path()).unwrap();
    store
        .publish(
            &graph,
            &store.leader().unwrap(),
            baleyg::model::IndexPin {
                index_generation: store.status().unwrap().revision.index_generation,
                index_revision: 0,
            },
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert!(
        store
            .publish(
                &graph,
                &store.leader().unwrap(),
                baleyg::model::IndexPin {
                    index_generation: store.status().unwrap().revision.index_generation,
                    index_revision: 1
                },
                &Arc::new(AtomicBool::new(true))
            )
            .is_err()
    );
}
#[test]
fn recovery_annotations_and_cancellation_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let source =
        "def healthy(x: annotation() = default()) -> returned():\n    café()\ndef broken(\n";
    fs::write(dir.path().join("bad.py"), source).unwrap();
    let options = IndexOptions::new(dir.path().to_owned());
    let graph = run(&options);
    assert_eq!(graph.stats.parse_error_files, 1);
    assert!(graph.nodes.iter().any(|n| n.name == "healthy"));
    assert!(
        !graph
            .calls
            .iter()
            .any(|c| ["annotation", "returned"].contains(&c.callee_text.as_str()))
    );
    assert!(graph.calls.iter().any(|c| c.callee_text == "café"));
    assert!(index_workspace(&options, &Arc::new(AtomicBool::new(true)), |_| {}).is_err());
    fs::remove_file(dir.path().join("bad.py")).unwrap();
    assert_eq!(graph.files[0].text, source);
    assert!(
        graph
            .diagnostics
            .iter()
            .any(|d| d.code == "python-lexical-only")
    );
}
