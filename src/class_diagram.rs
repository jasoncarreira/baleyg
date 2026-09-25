//! Bounded, presentation-only class diagrams. Stored relation certainty is never changed.
use crate::classes::{ClassDefinition, ClassRelation};
use crate::model::IndexPin;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_NODES: usize = 24;
pub const MAX_EDGES: usize = 64;
pub const MAX_EXPANDED: usize = 12;
/// Presentation limits; the persisted catalog retains all recorded evidence.
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const CLASS_BYTES: usize = 64 * 1024;
pub(crate) const PAGE_BYTES: usize = 3 * 1024 * 1024;
pub(crate) const RELATION_BYTES: usize = 64 * 1024;
pub(crate) const BYTE_NOTICE: &str = "Presentation byte limits omitted some class members or relationships; stored source evidence is unchanged.";
pub const INDEX_NOTICE: &str = "Index workspace to build class diagrams for this snapshot.";

#[derive(Debug)]
pub struct InvalidRequest(pub &'static str);
impl std::fmt::Display for InvalidRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for InvalidRequest {}
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 8192 && !id.contains('\0')
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassDiagramRequest {
    pub seed: String,
    pub expected_revision: IndexPin,
    #[serde(default)]
    pub expanded: Vec<String>,
    #[serde(default)]
    pub include_unmatched: bool,
    #[serde(default)]
    pub include_hierarchy: bool,
}
impl ClassDiagramRequest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            valid_id(&self.seed),
            InvalidRequest("Choose a valid class or method ID.")
        );
        ensure!(
            self.expanded.len() <= MAX_EXPANDED && self.expanded.iter().all(|id| valid_id(id)),
            InvalidRequest("Choose at most 12 valid related classes to expand.")
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassDiagramNode {
    pub id: String,
    pub class: Option<ClassDefinition>,
    pub label: String,
    pub kind: String,
    pub expandable: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassDiagram {
    pub revision: IndexPin,
    pub seed: String,
    pub nodes: Vec<ClassDiagramNode>,
    pub edges: Vec<ClassRelation>,
    pub warnings: Vec<String>,
    pub truncated: bool,
    pub require_index: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassPage {
    pub revision: IndexPin,
    pub items: Vec<ClassDefinition>,
    pub next_offset: Option<usize>,
    pub truncated: bool,
    pub warnings: Vec<String>,
    pub require_index: bool,
}
impl ClassDiagram {
    pub fn unindexed(revision: IndexPin, seed: String) -> Self {
        Self {
            revision,
            seed,
            nodes: vec![],
            edges: vec![],
            warnings: vec![INDEX_NOTICE.into()],
            truncated: false,
            require_index: true,
        }
    }
}

/// `seeds` starts with the focus class, then expansions in user order. `bridges`
/// connects each expansion to an earlier seed, so caps cannot strand an expansion.
/// Relations are already bounded by the store, with actual links before hints.
pub(crate) fn project(
    revision: IndexPin,
    seeds: &[String],
    classes: &BTreeMap<String, ClassDefinition>,
    bridges: Vec<ClassRelation>,
    relations: Vec<ClassRelation>,
    mut warnings: Vec<String>,
    mut truncated: bool,
) -> Result<ClassDiagram> {
    let mandatory: BTreeSet<_> = bridges.iter().map(|edge| edge.id.clone()).collect();
    let mut nodes = Vec::new();
    let mut ids = BTreeSet::new();
    let class_node = |class: &ClassDefinition| ClassDiagramNode {
        id: class.symbol.id.clone(),
        label: class.qualified_name.clone(),
        class: Some(class.clone()),
        kind: "class".into(),
        expandable: true,
    };
    for seed in seeds {
        if let Some(class) = classes.get(seed)
            && ids.insert(seed.clone())
        {
            nodes.push(class_node(class));
        }
    }
    // Count exact compact JSON for nodes/edges, with headroom for the envelope
    // and final notices. Payload loaders already cap each individual record.
    let mut response_bytes = serde_json::to_vec(&nodes).expect("class JSON").len()
        + serde_json::to_vec(&warnings).expect("warning JSON").len()
        + serde_json::to_vec(&seeds[0]).expect("seed JSON").len()
        + 4096;
    let mut byte_limited = false;
    let mut edges = Vec::new();
    let mut edge_ids = BTreeSet::new();
    for mut relation in bridges.into_iter().chain(relations) {
        if edge_ids.contains(&relation.id) {
            continue;
        }
        if edges.len() >= MAX_EDGES {
            ensure!(
                !mandatory.contains(&relation.id),
                InvalidRequest(
                    "Selected classes need more than 64 connecting edges. Remove an expansion."
                )
            );
            truncated = true;
            continue;
        }
        let target = relation
            .target
            .clone()
            .unwrap_or_else(|| format!("class-hint:{}", relation.id));
        let mut additions = Vec::new();
        let mut valid = true;
        for id in [&relation.owner, &target] {
            if ids.contains(id)
                || additions
                    .iter()
                    .any(|node: &ClassDiagramNode| &node.id == id)
            {
                continue;
            }
            if let Some(class) = classes.get(id) {
                additions.push(class_node(class));
            } else if relation.target.is_none() && id == &target {
                additions.push(ClassDiagramNode {
                    id: id.clone(),
                    class: None,
                    label: relation.type_name.clone(),
                    kind: if relation.match_kind == "ambiguous" {
                        "ambiguous"
                    } else {
                        "unmatched"
                    }
                    .into(),
                    expandable: false,
                });
            } else {
                valid = false;
            }
        }
        if !valid {
            ensure!(
                !mandatory.contains(&relation.id),
                InvalidRequest("A selected class connection exceeds the presentation byte limit.")
            );
            truncated = true;
            continue;
        }
        if nodes.len() + additions.len() > MAX_NODES {
            ensure!(
                !mandatory.contains(&relation.id),
                InvalidRequest(
                    "Selected classes need more than 24 connecting nodes. Remove an expansion."
                )
            );
            truncated = true;
            continue;
        }
        // An omitted optional bridge must not create a disconnected island later.
        if !ids.contains(&relation.owner) && !ids.contains(&target) {
            ensure!(
                !mandatory.contains(&relation.id),
                InvalidRequest(
                    "A selected class connection could not be preserved. Remove an expansion."
                )
            );
            truncated = true;
            continue;
        }
        // Rebind only the response clone, not the stored uncertain relationship.
        relation.target = Some(target);
        let addition_bytes = additions
            .iter()
            .map(|node| serde_json::to_vec(node).expect("class node JSON").len() + 1)
            .sum::<usize>()
            + serde_json::to_vec(&relation)
                .expect("class relation JSON")
                .len()
            + 1;
        if response_bytes + addition_bytes > MAX_RESPONSE_BYTES {
            ensure!(
                !mandatory.contains(&relation.id),
                InvalidRequest(
                    "Selected class connections exceed the 4 MiB presentation byte limit. Remove an expansion."
                )
            );
            truncated = true;
            byte_limited = true;
            continue;
        }
        response_bytes += addition_bytes;
        for node in additions {
            ids.insert(node.id.clone());
            nodes.push(node);
        }
        edge_ids.insert(relation.id.clone());
        edges.push(relation);
    }
    if byte_limited && !warnings.iter().any(|warning| warning == BYTE_NOTICE) {
        warnings.push(BYTE_NOTICE.into());
    }
    if truncated {
        warnings.push(
            "Class diagram is partial: catalog, node (24), or edge (64) limits were reached."
                .into(),
        );
    }
    Ok(ClassDiagram {
        revision,
        seed: seeds[0].clone(),
        nodes,
        edges,
        warnings,
        truncated,
        require_index: false,
    })
}
