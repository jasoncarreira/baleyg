use baleyg::{
    indexer::{IndexOptions, index_workspace},
    model::*,
    planning::*,
    store::Store,
};
use std::{
    collections::BTreeSet,
    sync::{Arc, atomic::AtomicBool},
};
use tempfile::TempDir;
const CODE: &str = "function leaf() {}\nfunction helper() { leaf(); }\nfunction seed(flag) { if (flag) { helper(); helper(); } console.log(flag); }\n";
fn fixture(code: &str) -> (TempDir, TempDir, Store, Graph, QuestionRequest) {
    let work = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(work.path().join("a.js"), code).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut graph =
        index_workspace(&IndexOptions::new(work.path().into()), &cancel, |_| {}).unwrap();
    // Explicit synthetic semantic links for policy tests only. Native lexical
    // extraction deliberately does not resolve these names. Keep its canonical
    // IDs, source spans, regions and file snapshots; publish through Store validation.
    for call in &mut graph.calls {
        if matches!(call.callee_text.as_str(), "helper" | "leaf") {
            let target = graph
                .nodes
                .iter()
                .find(|n| n.name == call.callee_text)
                .unwrap();
            call.target = Some(target.id.clone());
            call.resolution = Resolution::Internal;
        }
    }
    let store = Store::open(state.path(), work.path()).unwrap();
    let revision = store.publish(&graph, Some(0), &cancel).unwrap();
    let request = serde_json::from_value(serde_json::json!({"seed":graph.nodes.iter().find(|n| n.name == "seed").unwrap().id, "question":"Where is helper called?", "expectedRevision":revision})).unwrap();
    (work, state, store, graph, request)
}
fn all(packet: &QuestionPacket, relevance: Relevance) -> SelectionEnvelope {
    SelectionEnvelope {
        packet_id: packet.packet_id.clone(),
        decisions: packet
            .context
            .calls
            .iter()
            .map(|c| CallDecision {
                candidate_id: c.id.clone(),
                relevance,
                display_score: None,
            })
            .collect(),
    }
}
#[test]
fn packet_snapshot_unique_candidates_complete_source_and_stable_hash() {
    let (work, _state, store, _graph, request) = fixture(CODE);
    let packet = prepare(&store, request.clone()).unwrap();
    assert_eq!(packet, prepare(&store, request.clone()).unwrap());
    assert_eq!(packet.packet_id.len(), 64);
    assert_eq!(packet.source_files.len(), 1);
    assert_eq!(packet.source_files[0].text, CODE);
    assert_eq!(packet.context.calls.len(), 4);
    assert_eq!(
        packet
            .context
            .calls
            .iter()
            .map(|c| &c.id)
            .collect::<BTreeSet<_>>()
            .len(),
        4
    );
    assert_eq!(packet.context.query.max_nodes, 80);
    assert_eq!(packet.context.query.max_calls, 300);
    std::fs::write(work.path().join("a.js"), "changed on disk, not indexed").unwrap();
    assert_eq!(packet, prepare(&store, request.clone()).unwrap());
    let mut request2 = request;
    request2.question.push('?');
    assert_ne!(
        packet.packet_id,
        prepare(&store, request2).unwrap().packet_id
    );
    let mut corrupted = packet.clone();
    corrupted.source_files[0].text.push(' ');
    assert!(preview(&corrupted).is_err());
}
#[test]
fn strict_requests_and_limits() {
    let (_, _, _, _, request) = fixture(CODE);
    assert_eq!(request.evidence_depth, 2);
    assert_eq!(request.max_visible, 5);
    assert!(!request.allow_deeper_display);
    let mut value = serde_json::to_value(&request).unwrap();
    value["context"] = serde_json::json!({});
    assert!(serde_json::from_value::<QuestionRequest>(value).is_err());
    let mut value = serde_json::to_value(&request).unwrap();
    value.as_object_mut().unwrap().remove("expectedRevision");
    assert!(serde_json::from_value::<QuestionRequest>(value).is_err());
    for (depth, max) in [(4, 5), (2, 0), (2, 13)] {
        let mut r = request.clone();
        r.evidence_depth = depth;
        r.max_visible = max;
        assert!(r.validate().is_err());
    }
}
#[test]
fn exact_coverage_rejects_unknown_duplicate_missing_and_wrong_packet() {
    let (_w, _s, store, _, request) = fixture(CODE);
    let p = prepare(&store, request).unwrap();
    let valid = all(&p, Relevance::Essential);
    let mut bad = valid.clone();
    bad.decisions.pop();
    assert!(assemble(&p, &bad, "manual").is_err());
    let mut bad = valid.clone();
    bad.decisions.push(bad.decisions[0].clone());
    assert!(assemble(&p, &bad, "manual").is_err());
    let mut bad = valid.clone();
    bad.decisions[0].candidate_id = "invented-edge".into();
    assert!(assemble(&p, &bad, "manual").is_err());
    let mut bad = valid;
    bad.packet_id = "wrong".into();
    assert!(assemble(&p, &bad, "manual").is_err());
}
#[test]
fn direct_default_source_order_budget_and_actual_regions() {
    let (_w, _s, store, graph, mut request) = fixture(CODE);
    let p = prepare(&store, request.clone()).unwrap();
    let v = assemble(&p, &all(&p, Relevance::Essential), "manual").unwrap();
    assert_eq!(v.calls.len(), 3);
    assert_eq!(v.policy_hidden_count, 1);
    assert!(
        v.calls
            .iter()
            .all(|c| c.caller == request.seed && graph.calls.contains(c))
    );
    assert!(
        v.calls
            .windows(2)
            .all(|w| w[0].range.start_byte <= w[1].range.start_byte)
    );
    assert!(!v.regions.is_empty());
    assert!(v.regions.iter().all(|r| graph.regions.contains(r)));
    request.max_visible = 1;
    let p = prepare(&store, request.clone()).unwrap();
    let mut selection = all(&p, Relevance::Essential);
    selection.decisions.reverse();
    let v = assemble(&p, &selection, "manual").unwrap();
    assert_eq!(v.calls[0].callee_text, "helper");
    assert_eq!(v.calls.len(), 1);
    assert_eq!(v.policy_hidden_count, 3);
    request.allow_deeper_display = true;
    request.max_visible = 12;
    let p = prepare(&store, request).unwrap();
    let v = assemble(&p, &all(&p, Relevance::Essential), "manual").unwrap();
    assert_eq!(v.calls.len(), 4);
    assert_eq!(v.policy_hidden_count, 0);
    assert_eq!(v.calls.first().unwrap().callee_text, "leaf");
}
#[test]
fn local_literal_only_uncertainty_and_no_budget_filling() {
    let (_w, _s, store, _, mut request) = fixture(CODE);
    request.focus_terms = vec!["leaf".into()];
    let p = prepare(&store, request.clone()).unwrap();
    let s = preview(&p).unwrap();
    let v = assemble(&p, &s, "localPreview").unwrap();
    assert!(v.calls.is_empty());
    assert_eq!(v.supporting_count, 1);
    assert_eq!(v.uncertain_count, 3);
    assert_eq!(v.nodes.len(), 1);
    assert!(v.regions.is_empty());
    request.focus_terms = vec!["businessMeaningNotInNames".into()];
    let p = prepare(&store, request).unwrap();
    let v = assemble(&p, &preview(&p).unwrap(), "localPreview").unwrap();
    assert_eq!(v.uncertain_count, 4);
    assert!(v.calls.is_empty());
    assert!(
        v.warnings
            .iter()
            .any(|w| w.contains("meaning was not resolved"))
    );
    assert!(v.warnings.iter().any(|w| w.contains("not Jev/ACP")));
    for relevance in [
        Relevance::Supporting,
        Relevance::Incidental,
        Relevance::Uncertain,
    ] {
        assert!(
            assemble(&p, &all(&p, relevance), "manual")
                .unwrap()
                .calls
                .is_empty()
        );
    }
}
#[test]
fn revision_drift_rejected_snapshot_remains_immutable() {
    let (_w, _s, store, graph, request) = fixture(CODE);
    let p = prepare(&store, request.clone()).unwrap();
    store
        .publish(
            &graph,
            Some(request.expected_revision),
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert!(
        prepare(&store, request)
            .unwrap_err()
            .to_string()
            .contains("revision conflict")
    );
    assert!(store.source_at("a.js", Some(p.revision)).is_err());
    // Offline immutable packets remain reproducible; HTTP owns the current-revision guard.
    assert_eq!(preview(&p).unwrap(), preview(&p).unwrap());
}
#[test]
fn complete_source_packet_size_limit_errors_without_truncating() {
    let code = format!("{CODE}\n/*{}*/", "x".repeat(1024 * 1024));
    let (_w, _s, store, _, request) = fixture(&code);
    assert!(
        prepare(&store, request)
            .unwrap_err()
            .to_string()
            .contains("1 MiB")
    );
}
#[test]
fn candidate_limit_reports_incomplete_evidence() {
    let code = format!("function seed() {{ {} }}", "console.log();".repeat(301));
    let (_w, _s, store, _, request) = fixture(&code);
    let p = prepare(&store, request).unwrap();
    assert_eq!(p.context.calls.len(), 300);
    assert!(p.context.truncated);
    assert!(p.warnings.iter().any(|w| w.contains("truncated")));
    assert_eq!(preview(&p).unwrap().decisions.len(), 300);
}

#[test]
fn assembly_preserves_boundaries_and_never_expands_callbacks() {
    let code = "class Runner {}\nfunction cb() { console.log('callback'); }\nfunction seed(flag) { if (flag) { while (flag) { new Runner(); external(cb); } } }";
    let (_w, _s, store, mut graph, mut request) = fixture(code);
    let class_id = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class)
        .unwrap()
        .id
        .clone();
    let callback_id = graph
        .nodes
        .iter()
        .find(|n| n.name == "cb")
        .unwrap()
        .id
        .clone();
    for call in &mut graph.calls {
        if call.callee_text == "Runner" {
            // Explicit synthetic semantic constructor link, not inferred by planning.
            call.resolution = Resolution::Internal;
            call.target = Some(class_id.clone());
        } else if call.callee_text == "external" {
            call.resolution = Resolution::External;
            call.callback_arguments = vec![callback_id.clone()];
        }
    }
    request.expected_revision = store
        .publish(
            &graph,
            Some(request.expected_revision),
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    let p = prepare(&store, request).unwrap();
    assert_eq!(p.context.calls.len(), 2);
    let v = assemble(&p, &all(&p, Relevance::Essential), "manual").unwrap();
    assert_eq!(v.calls.len(), 2);
    assert!(v.calls.iter().all(|c| graph.calls.contains(c)));
    assert!(v.nodes.iter().any(|n| n.id == class_id));
    assert!(!v.nodes.iter().any(|n| n.id == callback_id));
    assert!(v.regions.len() >= 2);
    for region in &v.regions {
        if let Some(parent) = &region.parent {
            assert!(v.regions.iter().any(|r| &r.id == parent));
        }
    }
    assert!(v.calls.iter().any(|c| c.resolution == Resolution::External
        && c.callback_arguments == vec![callback_id.clone()]));
}

#[test]
fn display_scores_rank_membership_but_output_stays_in_source_order() {
    let (_w, _s, store, _, mut request) =
        fixture("function seed() { resolve(); open(); rename(); }");
    request.max_visible = 2;
    let p = prepare(&store, request).unwrap();
    let mut s = all(&p, Relevance::Essential);
    for d in &mut s.decisions {
        let c = p
            .context
            .calls
            .iter()
            .find(|c| c.id == d.candidate_id)
            .unwrap();
        d.display_score = Some(match c.callee_text.as_str() {
            "resolve" => 0.52,
            "open" => 0.44,
            "rename" => 0.96,
            _ => unreachable!(),
        });
    }
    s.decisions.reverse();
    let v = assemble(&p, &s, "importedJev").unwrap();
    assert_eq!(
        v.calls
            .iter()
            .map(|c| c.callee_text.as_str())
            .collect::<Vec<_>>(),
        vec!["resolve", "rename"]
    );
    assert_eq!(v.policy_hidden_count, 1);
    assert!(
        s.decisions
            .iter()
            .all(|d| d.relevance == Relevance::Essential)
    );
    assert!(v.calls.iter().all(|c| p.context.calls.contains(c)));
    assert!(
        v.warnings
            .iter()
            .any(|w| w == "Relative display scores are ranking hints, not calibrated confidence")
    );
    let unscored = all(&p, Relevance::Essential);
    let v = assemble(&p, &unscored, "manual").unwrap();
    assert_eq!(
        v.calls
            .iter()
            .map(|c| c.callee_text.as_str())
            .collect::<Vec<_>>(),
        vec!["resolve", "open"]
    );
    assert!(
        !v.warnings
            .iter()
            .any(|w| w.contains("Relative display scores"))
    );
    let json = serde_json::to_value(&unscored.decisions[0]).unwrap();
    assert!(json.get("displayScore").is_none());
    assert_eq!(
        serde_json::from_value::<CallDecision>(json)
            .unwrap()
            .display_score,
        None
    );
    let mut tied = unscored.clone();
    tied.decisions[1].display_score = Some(0.0);
    assert_eq!(assemble(&p, &tied, "manual").unwrap().calls, v.calls);
}
#[test]
fn display_scores_reject_nonfinite_and_out_of_range_values() {
    let (_w, _s, store, _, request) = fixture(CODE);
    let p = prepare(&store, request).unwrap();
    for score in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.01, 1.01] {
        let mut s = all(&p, Relevance::Uncertain);
        s.decisions[0].display_score = Some(score);
        assert!(
            assemble(&p, &s, "manual")
                .unwrap_err()
                .to_string()
                .contains("displayScore")
        );
    }
    for score in [0.0, 1.0] {
        let mut s = all(&p, Relevance::Essential);
        s.decisions[0].display_score = Some(score);
        assert!(s.validate(&p).is_ok());
    }
    assert!(
        preview(&p)
            .unwrap()
            .decisions
            .iter()
            .all(|d| d.display_score.is_none())
    );
}
#[test]
fn scores_never_promote_lower_labels_or_override_direct_display_policy() {
    let (_w, _s, store, _, mut request) = fixture(CODE);
    request.max_visible = 1;
    for allow in [false, true] {
        request.allow_deeper_display = allow;
        let p = prepare(&store, request.clone()).unwrap();
        let mut s = all(&p, Relevance::Essential);
        for d in &mut s.decisions {
            let c = p
                .context
                .calls
                .iter()
                .find(|c| c.id == d.candidate_id)
                .unwrap();
            d.display_score = Some(if c.caller == request.seed { 0.0 } else { 1.0 });
        }
        let v = assemble(&p, &s, "manual").unwrap();
        assert_eq!(v.calls.len(), 1);
        assert_eq!(v.calls[0].caller, request.seed);
        assert_eq!(v.policy_hidden_count, 3);
    }
    request.allow_deeper_display = false;
    request.max_visible = 12;
    let p = prepare(&store, request).unwrap();
    for relevance in [
        Relevance::Supporting,
        Relevance::Incidental,
        Relevance::Uncertain,
    ] {
        let mut s = all(&p, relevance);
        for d in &mut s.decisions {
            d.display_score = Some(1.0);
            let c = p
                .context
                .calls
                .iter()
                .find(|c| c.id == d.candidate_id)
                .unwrap();
            if c.caller != p.request.seed {
                d.relevance = Relevance::Essential;
            }
        }
        let v = assemble(&p, &s, "manual").unwrap();
        assert!(v.calls.is_empty());
        assert_eq!(v.policy_hidden_count, 1);
    }
}
