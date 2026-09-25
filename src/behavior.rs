//! Language-neutral static possible-path sequence contract.
//! Adapters consume cached source and measured calls. This is not a runtime trace.
use crate::model::{CallSite, IndexPin, Resolution, SourceFile, SourceRange, Symbol};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SequenceView {
    pub revision: IndexPin,
    pub seed: Symbol,
    pub participants: Vec<Participant>,
    pub steps: Vec<SequenceStep>,
    pub warnings: Vec<String>,
    pub hidden_steps: usize,
    pub truncated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Participant {
    pub id: String,
    pub label: String,
    pub kind: String,
    #[serde(default)]
    pub identification: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SequenceStep {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub path: String,
    pub range: SourceRange,
    pub call_id: Option<String>,
    pub target: Option<String>,
    pub resolution: Option<Resolution>,
    pub children: Vec<SequenceStep>,
    pub alternate: Vec<SequenceStep>,
    pub hidden: bool,
}
pub fn build_sequence(
    revision: IndexPin,
    seed: &Symbol,
    file: &SourceFile,
    calls: &[CallSite],
    show_all: bool,
) -> Result<SequenceView> {
    match file.language.as_str() {
        "javascript" => crate::behavior_js::build(revision, seed, file, calls, show_all),
        "rust" => crate::behavior_rust::build(revision, seed, file, calls, show_all),
        "java" => crate::behavior_java::build(revision, seed, file, calls, show_all),
        "python" => crate::behavior_python::build(revision, seed, file, calls, show_all),
        _ => bail!("unsupported sequence language: {}", file.language),
    }
}
