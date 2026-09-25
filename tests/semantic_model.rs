use baleyg::model::{self, v1::*};
use serde_json::{Value, json};

fn sid() -> String {
    format!("sid:v1:{}", "a".repeat(32))
}
fn occ() -> String {
    format!("occ:v1:{}", "b".repeat(32))
}
fn hash() -> String {
    "c".repeat(64)
}
fn document() -> Value {
    json!({"sourceSetId":"set", "language":"rust", "path":"src/lib.rs"})
}
fn range() -> Value {
    json!({"start":0,"end":4})
}
fn anchor() -> Value {
    json!({"syntaxId":sid(),"document":document(),"capturedRevisionId":"rev","headerHash":hash(),"siblingGroupHash":hash(),"siblingCount":1,"identicalHeaderCount":1})
}

#[test]
fn shared_primitives_are_closed_and_use_safe_json_integers() {
    for (value, valid) in [
        (json!(0), true),
        (json!(9007199254740991_u64), true),
        (json!(9007199254740992_u64), false),
        (json!(-1), false),
        (json!(1.5), false),
        (json!("4"), false),
    ] {
        assert_eq!(
            serde_json::from_value::<UInt>(value.clone()).is_ok(),
            valid,
            "{value}"
        );
    }
    assert!(serde_json::from_value::<Range>(json!({"start":0,"end":4,"extra":0})).is_err());
    assert!(serde_json::from_value::<Range>(json!({"start":0})).is_err());
    assert!(serde_json::from_value::<Range>(json!({"start":5,"end":4})).is_err());
    for bad in [
        "/abs",
        "../outside",
        "src//lib.rs",
        "src/./lib.rs",
        "src\\lib.rs",
        "src/../lib.rs",
        "",
    ] {
        assert!(Path::new(bad).is_none(), "{bad}");
    }
    assert!(Path::new("src/é.rs").is_some());
    assert!(SyntaxId::new(occ()).is_none());
    assert!(OccurrenceId::new(sid()).is_none());
    assert!(Hash::new("A".repeat(64)).is_none());
    assert!(serde_json::from_value::<Language>(json!("typescript")).is_err());
    assert_eq!(
        serde_json::to_value(Kind::AnonymousFunction).unwrap(),
        "anonymousFunction"
    );
    assert_eq!(
        serde_json::to_value(PositionEncoding::UnicodeScalar).unwrap(),
        "unicodeScalar"
    );
    assert_eq!(
        serde_json::to_value(Resolution::Resolved).unwrap(),
        "resolved"
    );
}

#[test]
fn all_record_families_have_exact_required_keys_and_explicit_nulls() {
    let producer = json!({"id":"native","version":"1","executableHash":hash(),"kind":"native","languages":["rust"],"positionEncoding":"utf8"});
    let source_set = json!({"id":"set","rootId":"root","languages":["rust"],"dependencies":[]});
    let doc = json!({"key":document(),"revisionId":"rev","contentHash":hash(),"byteLength":4});
    let revision = json!({"id":"rev","sourceSetId":"set","documents":[doc],"toolchainHash":hash(),"configHash":hash(),"dependencyHash":hash()});
    let coverage = json!({"producerId":"native","language":"rust","sourceSetId":"set","documentPath":"src/lib.rs","revisionId":"rev","requested":true,"selected":true,"state":"complete","supportedRoles":[],"observedRoles":[],"diagnostic":null});
    let basis = json!({"producerId":"semantic","producerVersion":"1","producerHash":hash(),"artifactHash":hash(),"language":"rust","sourceSetId":"set","revisionId":"rev","sourceManifestHash":hash(),"toolchainHash":hash(),"configHash":hash(),"dependencyHash":hash(),"lookupDependencies":[]});
    let provenance = json!({"id":"p","producerId":"native","document":document(),"revisionId":"rev","contentHash":hash(),"evidenceKind":"measuredSyntax","basis":null,"freshness":"fresh"});
    let key = json!({"kind":"function","name":"demo","signature":null,"ordinal":0});
    let header = json!({"kind":"function","name":"demo","modifiers":[],"typeParameters":[],"parameters":[{"name":null,"type":null,"variadic":false}],"resultType":null,"bases":[]});
    let declaration = json!({"syntaxId":sid(),"document":document(),"revisionId":"rev","kind":"function","name":"demo","lookupKey":"demo","ancestors":[],"key":key,"range":range(),"nameRange":range(),"header":header,"provenanceId":"p"});
    let symbol_key =
        json!({"scheme":"scip","symbol":"scip rust demo","scope":"global","document":null});
    let internal =
        json!({"kind":"internal","syntaxId":sid(),"document":document(),"revisionId":"rev"});
    let external = json!({"kind":"external","symbol":symbol_key});
    let symbol =
        json!({"key":symbol_key,"displayName":null,"declarations":[external],"provenanceId":"p"});
    let measured = json!({"document":document(),"revisionId":"rev","contentHash":hash(),"range":range(),"kind":"declarationName"});
    let join = json!({"anchor":measured,"status":"exact","candidateIds":[sid()],"diagnostic":null});
    let binding = json!({"syntaxId":sid(),"symbols":[symbol_key],"join":join,"provenanceId":"p"});
    let relationship =
        json!({"kind":"extends","source":internal,"target":external,"provenanceId":"p"});
    let call = json!({"id":occ(),"ownerSyntaxId":sid(),"ordinal":0,"document":document(),"revisionId":"rev","range":range(),"calleeRange":null,"spelling":"obj[expr]","regionIds":[],"provenanceId":"p"});
    let control = json!({"id":occ(),"ownerSyntaxId":sid(),"ordinal":0,"document":document(),"revisionId":"rev","kind":"if","range":range(),"parentId":null,"arm":null,"provenanceId":"p"});
    let reference = json!({"id":occ(),"ownerSyntaxId":sid(),"ordinal":0,"document":document(),"revisionId":"rev","range":range(),"spelling":"demo","lookupKey":"demo","site":"use","roles":["read"],"resolution":"unresolved","declaredTarget":null,"candidates":[],"provenanceId":"p"});
    let call_binding = json!({"callId":null,"join":join,"resolution":"unresolved","declaredTarget":null,"candidates":[],"dispatch":"unknown","possibleDispatch":[],"possibleDispatchComplete":false,"staleTarget":null,"provenanceId":"p"});
    let continuity =
        json!({"fromRevisionId":"rev","toRevisionId":"next","state":"unknown","evidence":null});
    let result = json!({"status":"orphaned","targetId":null,"reason":"groupChanged"});
    macro_rules! check {
        ($ty:ty, $json:expr) => {{
            let value: Value = $json.clone();
            let typed: $ty = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(typed).unwrap(), value, stringify!($ty));
            let mut extra = value.clone();
            extra
                .as_object_mut()
                .unwrap()
                .insert("unexpected".into(), json!(true));
            assert!(
                serde_json::from_value::<$ty>(extra).is_err(),
                "unknown {}",
                stringify!($ty)
            );
            let mut missing = value.clone();
            let field = missing.as_object().unwrap().keys().next().unwrap().clone();
            missing.as_object_mut().unwrap().remove(&field);
            assert!(
                serde_json::from_value::<$ty>(missing).is_err(),
                "missing {field} in {}",
                stringify!($ty)
            );
        }};
    }
    check!(Producer, producer);
    check!(SourceSet, source_set);
    check!(DocumentKey, document());
    check!(Document, doc);
    check!(Revision, revision);
    check!(Coverage, coverage);
    check!(SemanticBasis, basis);
    check!(Provenance, provenance);
    check!(
        Signature,
        json!({"parameterTypes":[],"typeParameterCount":0,"variadic":false})
    );
    check!(Key, key);
    check!(Parameter, json!({"name":null,"type":null,"variadic":false}));
    check!(Header, header);
    check!(Declaration, declaration);
    check!(SymbolKey, symbol_key);
    check!(Symbol, symbol);
    check!(MeasuredAnchor, measured);
    check!(Join, join);
    check!(DeclarationBinding, binding);
    check!(TypeRelationship, relationship);
    check!(Call, call);
    check!(ControlRegion, control);
    check!(Reference, reference);
    check!(CallBinding, call_binding);
    check!(DurableAnchor, anchor());
    check!(GroupContinuity, continuity);
    check!(AnchorResult, result);
    check!(
        NativeRevisionContext,
        json!({"sourceSet":source_set,"revision":revision,"producer":producer})
    );
    check!(
        NativeFileEvidence,
        json!({"document":doc,"coverage":coverage,"provenance":provenance,"declarations":[],"calls":[],"controlRegions":[],"diagnostics":[]})
    );
    for target in [internal, external] {
        check!(Target, target);
    }
    assert!(
        serde_json::from_value::<Target>(
            json!({"kind":"external","symbol":symbol_key,"syntaxId":sid()})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<Join>(
            json!({"anchor":measured,"status":"exact","candidateIds":["legacy"],"diagnostic":null})
        )
        .is_err()
    );
}

#[test]
fn nullable_call_fields_are_independent_and_historical_payloads_remain_distinct() {
    let base = json!({"id":occ(),"ownerSyntaxId":sid(),"ordinal":0,"document":document(),"revisionId":"rev","range":range(),"calleeRange":null,"spelling":null,"regionIds":[],"provenanceId":"p"});
    for callee in [Value::Null, range()] {
        for spelling in [Value::Null, json!("foo")] {
            let mut value = base.clone();
            value["calleeRange"] = callee.clone();
            value["spelling"] = spelling.clone();
            let call: Call = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(call).unwrap(), value);
        }
    }
    let old = json!({"id":"v","title":"View","query":{"seed":"id","depth":1,"maxNodes":40,"maxCalls":200,"includeCallbacks":false,"excludePaths":[]},"pins":{},"hidden":[]});
    assert!(serde_json::from_value::<model::SavedView>(old.clone()).is_ok());
    assert!(serde_json::from_value::<SavedViewDurable>(old.clone()).is_err());
    assert!(serde_json::from_value::<SavedViewWrite>(old.clone()).is_ok());
    let mut modern = old.clone();
    modern["anchors"] = json!({sid():anchor()});
    assert!(serde_json::from_value::<SavedViewDurable>(modern.clone()).is_ok());
    assert!(serde_json::from_value::<SavedViewWrite>(modern).is_err());
    let historical_note = json!({"id":"n","nodeId":"legacy","body":"preserved"});
    assert!(serde_json::from_value::<model::Annotation>(historical_note.clone()).is_ok());
    assert!(serde_json::from_value::<AnnotationDurable>(historical_note.clone()).is_err());
    assert!(serde_json::from_value::<AnnotationWrite>(historical_note.clone()).is_ok());
    let mut modern_note = historical_note;
    modern_note["anchor"] = Value::Null;
    assert!(serde_json::from_value::<AnnotationDurable>(modern_note.clone()).is_ok());
    assert!(serde_json::from_value::<AnnotationWrite>(modern_note).is_err());
    assert!(serde_json::from_value::<AnchorReason>(json!("missingAnchor")).is_err());
    assert!(serde_json::from_value::<Attachment>(json!({"status":"missingAnchor"})).is_ok());
    let _: model::Graph = model::Graph::default();
    let _ = model::SourceFile {
        path: String::new(),
        hash: String::new(),
        language: String::new(),
        text: String::new(),
    };
    let _ = model::SavedView {
        id: String::new(),
        title: String::new(),
        query: serde_json::from_value(old["query"].clone()).unwrap(),
        pins: Default::default(),
        hidden: vec![],
    };
    let _ = model::Annotation {
        id: String::new(),
        node_id: String::new(),
        body: String::new(),
    };
}
