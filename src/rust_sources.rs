//! Explicit source candidates. Never added to the workspace graph or provider context.
use crate::{file_tree::SourceDir, model::*};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

pub struct Root {
    pub label: String,
    pub directory: SourceDir,
}
pub fn open_roots(roots: Vec<(String, PathBuf)>) -> anyhow::Result<Vec<Root>> {
    anyhow::ensure!(roots.len() <= 8, "too many Rust source roots");
    let mut labels = std::collections::BTreeSet::new();
    roots
        .into_iter()
        .map(|(label, path)| {
            anyhow::ensure!(
                !label.is_empty()
                    && label.len() <= 48
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                "invalid Rust source root label"
            );
            anyhow::ensure!(
                labels.insert(label.clone()),
                "duplicate Rust source root label"
            );
            Ok(Root {
                label,
                directory: SourceDir::open(&path)?,
            })
        })
        .collect()
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub id: String,
    pub root_id: String,
    pub root_label: String,
    pub path: String,
    pub hash: String,
    pub file: SourceFile,
    pub definitions: Vec<Symbol>,
    pub warnings: Vec<String>,
}
impl Root {
    pub fn snapshot(&self, path: &str) -> std::io::Result<Snapshot> {
        if !path.ends_with(".rs") || !crate::file_tree::valid_path(path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid Rust file",
            ));
        }
        let text = self.directory.read_file(path)?;
        let hash = hex::encode(Sha256::digest(text.as_bytes()));
        let mut identity = Sha256::new();
        for part in [
            self.label.as_bytes(),
            self.directory.root.as_os_str().as_encoded_bytes(),
            path.as_bytes(),
            text.as_bytes(),
        ] {
            identity.update((part.len() as u64).to_be_bytes());
            identity.update(part);
        }
        let id = format!("rust-source:{}", hex::encode(identity.finalize()));
        let file = SourceFile {
            path: path.into(),
            hash: hash.clone(),
            language: "rust".into(),
            text,
        };
        let mut graph = Graph::default();
        let mut warnings = vec!["Definitional candidates only; not confirmed callees. Separate from the workspace graph and source-sharing scope.".into()];
        if crate::indexer_rust::extract_external(
            &mut graph,
            &file,
            &Arc::new(AtomicBool::new(false)),
        )
        .is_err()
        {
            warnings.push("Rust extraction failed; definitions may be incomplete.".into());
        }
        warnings.extend(graph.diagnostics.into_iter().map(|d| d.message));
        let mut definitions: Vec<_> = graph
            .nodes
            .into_iter()
            .filter(|s| s.kind != SymbolKind::Module)
            .collect();
        if definitions.len() > 2000 {
            definitions.truncate(2000);
            warnings.push("Definitions truncated to 2000 candidates.".into());
        }
        Ok(Snapshot {
            id,
            root_id: self.label.clone(),
            root_label: self.label.clone(),
            path: path.into(),
            hash,
            file,
            definitions,
            warnings,
        })
    }
}
