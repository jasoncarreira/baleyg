//! Immutable, local declaration catalogs. Never merged with workspace graphs.
use crate::model::IndexPin;
use crate::model::SourceRange;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Package {
    pub id: String,
    pub ecosystem: String,
    pub name: String,
    pub version: String,
    pub source: String,
    pub aliases: Vec<String>,
    pub source_state: String,
    pub index_state: String,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSymbol {
    pub id: String,
    pub package_id: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub parent: Option<String>,
    pub owner_expression: Option<String>,
    pub signature: String,
    pub source_ref: String,
    pub path: String,
    pub range: SourceRange,
}
#[derive(Clone)]
pub struct CatalogSource {
    /// Trusted authority, never a manifest-selected package directory.
    pub root: PathBuf,
    pub directory: std::sync::Arc<crate::file_tree::SourceDir>,
    pub path: String,
    pub hash: String,
    pub package_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub id: String,
    pub workspace_revision: IndexPin,
    pub packages: Vec<Package>,
    pub symbols: Vec<CatalogSymbol>,
    pub warnings: Vec<String>,
    #[serde(skip)]
    pub sources: HashMap<String, CatalogSource>,
}
#[derive(Debug, Clone, Default)]
pub struct CatalogOptions {
    pub cargo_home: Option<PathBuf>,
    pub rust_library: Option<PathBuf>,
}
impl Catalog {
    pub fn build(
        workspace: &Path,
        revision: IndexPin,
        options: &CatalogOptions,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Self> {
        crate::dependency_rust::build(workspace, revision, options, cancel)
    }
}

impl std::fmt::Debug for CatalogSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogSource")
            .field("path", &self.path)
            .field("hash", &self.hash)
            .finish_non_exhaustive()
    }
}
