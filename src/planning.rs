//! Bounded, offline question evidence and selection policy. No model or network calls.
use crate::model::IndexPin;
use crate::{model::*, store::Store};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const MAX_PACKET_BYTES: usize = 1024 * 1024;
fn default_depth() -> usize {
    2
}
fn default_visible() -> usize {
    5
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuestionRequest {
    pub seed: String,
    pub question: String,
    pub expected_revision: IndexPin,
    #[serde(default = "default_depth")]
    pub evidence_depth: usize,
    #[serde(default = "default_visible")]
    pub max_visible: usize,
    #[serde(default)]
    pub allow_deeper_display: bool,
    #[serde(default)]
    pub focus_terms: Vec<String>,
}
impl QuestionRequest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.seed.trim().is_empty() && self.seed.len() <= 8192,
            "seed must contain 1..8192 bytes"
        );
        ensure!(
            !self.question.trim().is_empty() && self.question.len() <= 8192,
            "question must contain 1..8192 bytes"
        );
        ensure!(self.evidence_depth <= 3, "evidenceDepth must be at most 3");
        ensure!(
            (1..=12).contains(&self.max_visible),
            "maxVisible must be 1..12"
        );
        ensure!(
            self.focus_terms.len() <= 12
                && self
                    .focus_terms
                    .iter()
                    .all(|t| !t.trim().is_empty() && t.len() <= 256),
            "focusTerms must contain at most 12 nonempty terms of at most 256 bytes"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct QuestionPacket {
    pub packet_id: String,
    pub revision: IndexPin,
    pub request: QuestionRequest,
    pub context: ViewResult,
    pub source_files: Vec<SourceFile>,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Relevance {
    Essential,
    Supporting,
    Incidental,
    Uncertain,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallDecision {
    pub candidate_id: String,
    pub relevance: Relevance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_score: Option<f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectionEnvelope {
    pub packet_id: String,
    pub decisions: Vec<CallDecision>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FocusedView {
    pub packet_id: String,
    pub revision: IndexPin,
    pub question: String,
    pub selection_source: String,
    pub nodes: Vec<Symbol>,
    pub calls: Vec<CallSite>,
    pub regions: Vec<ControlRegion>,
    pub supporting_count: usize,
    pub omitted_count: usize,
    pub uncertain_count: usize,
    pub policy_hidden_count: usize,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QuestionPreview {
    pub packet: QuestionPacket,
    pub selection: SelectionEnvelope,
    pub view: FocusedView,
}

fn packet_id(packet: &QuestionPacket) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&(
        "baleyg-question-v1",
        &packet.request,
        packet.revision,
        &packet.context,
        &packet.source_files,
        &packet.warnings,
    ))?)))
}
impl QuestionPacket {
    /// Detect accidental mutation; this is not authentication of client-supplied graph data.
    pub fn validate(&self) -> Result<()> {
        self.request.validate()?;
        ensure!(
            self.revision == self.request.expected_revision
                && self.revision == self.context.revision,
            "revision conflict in packet"
        );
        ensure!(
            self.context.query.seed == self.request.seed,
            "packet seed mismatch"
        );
        ensure!(
            self.packet_id == packet_id(self)?,
            "packet integrity mismatch"
        );
        ensure!(
            serde_json::to_vec(self)?.len() <= MAX_PACKET_BYTES,
            "complete question packet exceeds 1 MiB"
        );
        Ok(())
    }
}
pub fn prepare(store: &Store, request: QuestionRequest) -> Result<QuestionPacket> {
    request.validate()?;
    ensure!(
        store.status()?.revision == request.expected_revision,
        "revision conflict: requested snapshot is no longer current"
    );
    let context = store
        .query_view(&ViewQuery {
            seed: request.seed.clone(),
            depth: request.evidence_depth,
            max_nodes: 80,
            max_calls: 300,
            include_callbacks: false,
            exclude_paths: vec![],
        })?
        .context("question seed not found")?;
    ensure!(
        context.revision == request.expected_revision,
        "revision conflict: graph changed during question preparation"
    );
    let paths: BTreeSet<_> = context
        .nodes
        .iter()
        .map(|n| n.path.as_str())
        .chain(context.calls.iter().map(|c| c.path.as_str()))
        .chain(context.regions.iter().map(|r| r.path.as_str()))
        .collect();
    let mut source_files = Vec::new();
    let mut bytes = 0usize;
    for path in paths {
        let (_, file) = store
            .source_at(path, Some(request.expected_revision))?
            .context("missing indexed source file")?;
        bytes = bytes.saturating_add(serde_json::to_vec(&file)?.len());
        ensure!(
            bytes <= MAX_PACKET_BYTES,
            "complete question packet exceeds 1 MiB; sources are never truncated"
        );
        source_files.push(file);
    }
    ensure!(
        store.status()?.revision == request.expected_revision,
        "revision conflict: graph changed during question preparation"
    );
    let mut warnings = context.warnings.clone();
    warnings.push(format!("Bounded evidence only: depth {}, at most 80 nodes and 300 calls; callbacks are not expanded. This is not a complete program or answer.", request.evidence_depth));
    let mut packet = QuestionPacket {
        packet_id: String::new(),
        revision: context.revision,
        request,
        context,
        source_files,
        warnings,
    };
    packet.packet_id = packet_id(&packet)?;
    packet.validate()?;
    Ok(packet)
}
fn terms(request: &QuestionRequest) -> Vec<String> {
    if !request.focus_terms.is_empty() {
        return request
            .focus_terms
            .iter()
            .map(|t| t.trim().to_lowercase())
            .collect();
    }
    request
        .question
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .map(str::to_lowercase)
        .filter(|t| {
            t.len() > 2
                && !matches!(
                    t.as_str(),
                    "the"
                        | "and"
                        | "are"
                        | "does"
                        | "how"
                        | "what"
                        | "where"
                        | "when"
                        | "which"
                        | "why"
                        | "this"
                        | "that"
                        | "with"
                        | "from"
                        | "for"
                        | "into"
                        | "calls"
                        | "call"
                        | "function"
                )
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
fn literal_match(call: &CallSite, terms: &[String]) -> bool {
    let text = call.callee_text.to_lowercase();
    terms.iter().any(|term| text.contains(term))
}
/// LOCAL literal callee-name matching only. Does not interpret the question or source semantics.
pub fn preview(packet: &QuestionPacket) -> Result<SelectionEnvelope> {
    packet.validate()?;
    let terms = terms(&packet.request);
    let mut calls: Vec<_> = packet.context.calls.iter().collect();
    calls.sort_by_key(|call| call.caller != packet.request.seed);
    Ok(SelectionEnvelope {
        packet_id: packet.packet_id.clone(),
        decisions: calls
            .into_iter()
            .map(|call| CallDecision {
                candidate_id: call.id.clone(),
                display_score: None,
                relevance: if literal_match(call, &terms) {
                    if call.caller == packet.request.seed || packet.request.allow_deeper_display {
                        Relevance::Essential
                    } else {
                        Relevance::Supporting
                    }
                } else {
                    Relevance::Uncertain
                },
            })
            .collect(),
    })
}
impl SelectionEnvelope {
    pub fn validate(&self, packet: &QuestionPacket) -> Result<()> {
        packet.validate()?;
        ensure!(
            self.packet_id == packet.packet_id,
            "selection packetId mismatch"
        );
        let candidates: BTreeSet<_> = packet.context.calls.iter().map(|c| c.id.as_str()).collect();
        ensure!(
            candidates.len() == packet.context.calls.len(),
            "duplicate context call IDs"
        );
        let mut seen = BTreeSet::new();
        for decision in &self.decisions {
            ensure!(
                decision
                    .display_score
                    .is_none_or(|score| score.is_finite() && (0.0..=1.0).contains(&score)),
                "displayScore must be finite and within 0..1"
            );
            ensure!(
                candidates.contains(decision.candidate_id.as_str()),
                "unknown candidate ID"
            );
            ensure!(
                seen.insert(decision.candidate_id.as_str()),
                "duplicate candidate decision"
            );
        }
        ensure!(seen == candidates, "missing candidate decisions");
        Ok(())
    }
}
pub fn assemble(
    packet: &QuestionPacket,
    selection: &SelectionEnvelope,
    source: &str,
) -> Result<FocusedView> {
    selection.validate(packet)?;
    let decisions: BTreeMap<_, _> = selection
        .decisions
        .iter()
        .map(|d| (d.candidate_id.as_str(), d.relevance))
        .collect();
    let mut eligible: Vec<_> = packet
        .context
        .calls
        .iter()
        .filter(|c| decisions[c.id.as_str()] == Relevance::Essential)
        .collect();
    let essential_count = eligible.len();
    eligible.retain(|c| packet.request.allow_deeper_display || c.caller == packet.request.seed);
    // Direct seed calls win first; scores only rank essential calls within policy.
    let scores: BTreeMap<_, _> = selection
        .decisions
        .iter()
        .map(|d| (d.candidate_id.as_str(), d.display_score.unwrap_or(0.0)))
        .collect();
    let source_order = |a: &&CallSite, b: &&CallSite| {
        (
            &a.path,
            a.range.start_byte,
            std::cmp::Reverse(a.range.end_byte),
            &a.id,
        )
            .cmp(&(
                &b.path,
                b.range.start_byte,
                std::cmp::Reverse(b.range.end_byte),
                &b.id,
            ))
    };
    eligible.sort_by(|a, b| {
        (a.caller != packet.request.seed)
            .cmp(&(b.caller != packet.request.seed))
            .then_with(|| {
                scores[b.id.as_str()]
                    .partial_cmp(&scores[a.id.as_str()])
                    .expect("validated finite scores")
            })
            .then_with(|| source_order(a, b))
    });
    eligible.truncate(packet.request.max_visible);
    // Ranking chooses membership, not a synthetic execution order.
    eligible.sort_by(source_order);
    let policy_hidden_count = essential_count - eligible.len();
    let calls: Vec<CallSite> = eligible.into_iter().cloned().collect();
    let mut node_ids = BTreeSet::from([packet.request.seed.as_str()]);
    let mut region_ids = BTreeSet::new();
    let regions_by_id: BTreeMap<_, _> = packet
        .context
        .regions
        .iter()
        .map(|r| (r.id.as_str(), r))
        .collect();
    for call in &calls {
        node_ids.insert(call.caller.as_str());
        if call.resolution == Resolution::Internal
            && let Some(target) = &call.target
        {
            node_ids.insert(target.as_str());
        }
        for id in &call.regions {
            let mut current = Some(id.as_str());
            while let Some(id) = current {
                if !region_ids.insert(id) {
                    break;
                }
                current = regions_by_id.get(id).and_then(|r| r.parent.as_deref());
            }
        }
    }
    let mut warnings = packet.warnings.clone();
    if selection
        .decisions
        .iter()
        .any(|d| d.display_score.is_some())
    {
        warnings
            .push("Relative display scores are ranking hints, not calibrated confidence".into());
    }
    warnings.push("Selection is relevance evidence, not proof of an answer; unresolved, external, class and callback boundaries remain unchanged.".into());
    if source == "localPreview" || source == "local" || source == "preview" {
        warnings.push("Local preview—not Jev/ACP: deterministic literal callee-name matching only; no question understanding or semantic completeness claim.".into());
        let terms = terms(&packet.request);
        let unmatched: Vec<_> = terms
            .iter()
            .filter(|t| {
                !packet
                    .context
                    .calls
                    .iter()
                    .any(|c| c.callee_text.to_lowercase().contains(t.as_str()))
            })
            .cloned()
            .collect();
        if terms.is_empty() || !unmatched.is_empty() {
            warnings.push("Some question/focus terms have no literal callee-name match. Their meaning was not resolved; use manual selection or review evidence.".into());
        }
    }
    if policy_hidden_count > 0 {
        warnings.push(format!("Display policy hid {policy_hidden_count} essential calls (direct-only policy or maxVisible); no lower-relevance calls were promoted."));
    }
    let nodes = packet
        .context
        .nodes
        .iter()
        .filter(|n| node_ids.contains(n.id.as_str()))
        .cloned()
        .collect();
    let regions = packet
        .context
        .regions
        .iter()
        .filter(|r| region_ids.contains(r.id.as_str()))
        .cloned()
        .collect();
    Ok(FocusedView {
        packet_id: packet.packet_id.clone(),
        revision: packet.revision,
        question: packet.request.question.clone(),
        selection_source: source.into(),
        nodes,
        regions,
        omitted_count: packet.context.calls.len() - calls.len(),
        calls,
        supporting_count: decisions
            .values()
            .filter(|r| **r == Relevance::Supporting)
            .count(),
        uncertain_count: decisions
            .values()
            .filter(|r| **r == Relevance::Uncertain)
            .count(),
        policy_hidden_count,
        warnings,
    })
}
