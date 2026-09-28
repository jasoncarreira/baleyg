use baleyg::{
    model::{AnchorReason, AnchorStatus},
    native_evidence::{Declaration, DocumentKey, Header, Key, Range},
    store::anchors::{ContinuityState, GroupContinuity, audit_anchor, capture_anchor},
};

fn declaration(id: &str, revision: &str, ordinal: usize, start: usize, name: &str) -> Declaration {
    Declaration {
        syntax_id: id.into(),
        document: DocumentKey { source_set_id: "source-set:v1:test".into(), language: "rust".into(), path: "src/lib.rs".into() },
        revision_id: revision.into(), kind: "function".into(), name: Some(name.into()), lookup_key: Some(name.into()),
        ancestors: vec![], key: Key { kind: "function".into(), name: Some(name.into()), signature: None, ordinal },
        range: Range { start, end: start + 10 }, name_range: Some(Range { start, end: start + name.len() }),
        header: Header { kind: "function".into(), name: Some(name.into()), modifiers: vec!["pub".into()], type_parameters: vec![], parameters: vec![], result_type: None, bases: vec![] },
        provenance_id: "provenance:v1:test".into(),
    }
}

#[test]
fn worked_audit_vectors() {
    let id = "sid:v1:0123456789abcdef0123456789abcdef";
    let old = declaration(id, "rev-old", 0, 0, "run");
    let anchor = capture_anchor(&old, std::slice::from_ref(&old)).unwrap();
    assert_eq!(anchor.header_hash, "4d0e96c70ccf4bcb926126d35c124bc412341b3040a25c54719a4ba1586396a2");
    assert_eq!(anchor.sibling_group_hash, "73647a072adc42d17950fc2419ffb2d878c09de37277d1d2bd21264ec72a58e8");
    let same = audit_anchor(&anchor, "rev-old", Some(&old), std::slice::from_ref(&old), None).unwrap();
    assert_eq!(same.status, AnchorStatus::Attached);
    assert_eq!(same.target_id.as_deref(), Some(id));

    let current = declaration(id, "rev-new", 0, 0, "run");
    let body_only = audit_anchor(&anchor, "rev-new", Some(&current), std::slice::from_ref(&current), None).unwrap();
    assert_eq!(body_only.status, AnchorStatus::Attached, "unique headers do not require group continuity");

    let id2 = "sid:v1:abcdef0123456789abcdef0123456789";
    let duplicate = declaration(id2, "rev-old", 1, 20, "run");
    let old_group = vec![old.clone(), duplicate];
    let duplicate_anchor = capture_anchor(&old, &old_group).unwrap();
    let current2 = declaration(id2, "rev-new", 1, 20, "run");
    let current_group = vec![current.clone(), current2];
    let unknown = audit_anchor(&duplicate_anchor, "rev-new", Some(&current), &current_group, None).unwrap();
    assert_eq!(unknown.reason, AnchorReason::UnprovenContinuity);
    let witness = GroupContinuity { from_revision_id: "rev-old".into(), to_revision_id: "rev-new".into(), state: ContinuityState::Unchanged, evidence: Some("independent source diff witness".into()) };
    assert_eq!(audit_anchor(&duplicate_anchor, "rev-new", Some(&current), &current_group, Some(&witness)).unwrap().status, AnchorStatus::Attached);
}

#[test]
fn ordered_failure_precedence() {
    let id = "sid:v1:0123456789abcdef0123456789abcdef";
    let old = declaration(id, "a", 0, 0, "run");
    let anchor = capture_anchor(&old, std::slice::from_ref(&old)).unwrap();
    assert_eq!(audit_anchor(&anchor, "b", None, &[], None).unwrap().reason, AnchorReason::Missing);
    let mut changed = declaration(id, "b", 0, 0, "run");
    changed.header.modifiers.push("async".into());
    assert_eq!(audit_anchor(&anchor, "b", Some(&changed), std::slice::from_ref(&changed), None).unwrap().reason, AnchorReason::HeaderMismatch);
}

#[test]
fn invalid_cross_revision_witness_is_rejected() {
    let id = "sid:v1:0123456789abcdef0123456789abcdef";
    let old = declaration(id, "a", 0, 0, "run");
    let anchor = capture_anchor(&old, std::slice::from_ref(&old)).unwrap();
    let current = declaration(id, "b", 0, 0, "run");
    let invalid = GroupContinuity { from_revision_id: "a".into(), to_revision_id: "b".into(), state: ContinuityState::Unchanged, evidence: Some(" ".into()) };
    assert!(audit_anchor(&anchor, "b", Some(&current), std::slice::from_ref(&current), Some(&invalid)).is_err());
}
