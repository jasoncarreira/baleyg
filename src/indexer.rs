//! Native-only graph projection from one immutable capture and validated #22 evidence.
//! Syntax never supplies a target, candidate edge, dispatch resolution or callback traversal.
use crate::{capture::Capture, model::*, native_evidence};
use anyhow::{Context, Result, ensure};
use protobuf::Message;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::atomic::Ordering,
};
use tree_sitter::Node;

#[derive(Clone, Debug)]
pub struct IndexOptions {
    pub workspace_root: PathBuf,
    pub scip_path: Option<PathBuf>,
    pub manifest_path: Option<PathBuf>,
    pub max_file_bytes: u64,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReconcileOptions {
    pub version: u8,
    pub max_file_bytes: u64,
    pub scip_path: Option<String>,
    pub manifest_path: Option<String>,
}
impl From<&IndexOptions> for ReconcileOptions {
    fn from(options: &IndexOptions) -> Self {
        Self {
            version: 1,
            max_file_bytes: options.max_file_bytes.min(256 * 1024 * 1024),
            scip_path: options
                .scip_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            manifest_path: options
                .manifest_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
        }
    }
}

impl IndexOptions {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            scip_path: None,
            manifest_path: None,
            max_file_bytes: 2 * 1024 * 1024,
        }
    }
}

fn root_id(root: &std::path::Path) -> Result<String> {
    Ok(crate::store::topology::WorkspaceIdentity::discover(Some(root), root)?.record_id)
}

pub fn index_workspace(
    options: &IndexOptions,
    cancel: &CancelFlag,
    progress: impl Fn(IndexProgress) + Sync,
) -> Result<Graph> {
    let (graph, _) = index_workspace_with_capture(options, cancel, progress)?;
    Ok(graph)
}

/// Return the original admitted descriptor for verification at the publication cutoff.
pub fn index_workspace_with_capture(
    options: &IndexOptions,
    cancel: &CancelFlag,
    progress: impl Fn(IndexProgress) + Sync,
) -> Result<(Graph, Capture)> {
    let capture = Capture::admit(options, cancel, &progress)?;
    let root = std::fs::canonicalize(&options.workspace_root)?;
    let native = native_evidence::from_capture(&capture, &root, &root_id(&root)?, cancel)?;
    let graph = project_native(options, &capture, &native, cancel, &progress)?;
    capture.verify(cancel)?;
    Ok((graph, capture))
}

/// Both views are built from the identical validated immutable capture.
pub fn index_workspace_with_native(
    options: &IndexOptions,
    root_id: &str,
    cancel: &CancelFlag,
    progress: impl Fn(IndexProgress) + Sync,
) -> Result<(Graph, native_evidence::Artifact)> {
    let capture = Capture::admit(options, cancel, &progress)?;
    let root = std::fs::canonicalize(&options.workspace_root)?;
    let native = native_evidence::from_capture(&capture, &root, root_id, cancel)?;
    let graph = project_native(options, &capture, &native, cancel, &progress)?;
    capture.verify(cancel)?;
    Ok((graph, native))
}

/// One admission produces the graph and native artifact for the same immutable capture.
pub fn index_workspace_bundle(
    options: &IndexOptions,
    root_id: &str,
    cancel: &CancelFlag,
    progress: impl Fn(IndexProgress) + Sync,
) -> Result<(Graph, native_evidence::Artifact, Capture)> {
    let capture = Capture::admit(options, cancel, &progress)?;
    let root = std::fs::canonicalize(&options.workspace_root)?;
    let native = native_evidence::from_capture(&capture, &root, root_id, cancel)?;
    let graph = project_native(options, &capture, &native, cancel, &progress)?;
    capture.verify(cancel)?;
    Ok((graph, native, capture))
}

fn location(text: &str, byte: usize) -> Result<(usize, usize)> {
    ensure!(
        byte <= text.len() && text.is_char_boundary(byte),
        "invalid measured graph range"
    );
    let prefix = &text[..byte];
    Ok((
        prefix.bytes().filter(|c| *c == b'\n').count() + 1,
        byte - prefix.rfind('\n').map_or(0, |i| i + 1) + 1,
    ))
}
fn measured_range(file: &SourceFile, range: &native_evidence::Range) -> Result<SourceRange> {
    let (start_line, start_column) = location(&file.text, range.start)?;
    let (end_line, end_column) = location(&file.text, range.end)?;
    ensure!(range.start <= range.end, "inverted measured graph range");
    Ok(SourceRange {
        start_byte: range.start,
        end_byte: range.end,
        start_line,
        start_column,
        end_line,
        end_column,
    })
}
fn graph_kind(kind: &str) -> Result<SymbolKind> {
    // Exhaust the validated #22 declaration vocabulary. Unknown kinds must fail closed,
    // rather than silently inventing a callable graph node.
    Ok(match kind {
        "module" | "namespace" => SymbolKind::Module,
        "type" | "implementation" => SymbolKind::Class,
        "method" | "constructor" => SymbolKind::Method,
        "function" | "anonymousFunction" => SymbolKind::Function,
        "field" => SymbolKind::Field,
        "variable" => SymbolKind::Variable,
        "parameter" => SymbolKind::Parameter,
        "typeParameter" => SymbolKind::TypeParameter,
        "alias" => SymbolKind::Alias,
        _ => anyhow::bail!("unknown native declaration kind: {kind}"),
    })
}
fn graph_provenance() -> Provenance {
    Provenance {
        source: "native-measuredSyntax".into(),
        semantic: SemanticState::Unavailable,
    }
}
/// A SCIP symbol is strictly untrusted UI copy, keyed only to a captured native SyntaxId.
/// A malformed/missing/stale optional display artifact contributes no labels.
fn measured_display_labels(
    options: &IndexOptions,
    capture: &Capture,
    native: &native_evidence::Artifact,
) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    let (Some(scip_path), Some(manifest_path)) = (&options.scip_path, &options.manifest_path)
    else {
        return labels;
    };
    let Some((index, manifest)) = capture
        .bytes(scip_path)
        .and_then(|bytes| scip::types::Index::parse_from_bytes(bytes).ok())
        .zip(
            capture
                .bytes(manifest_path)
                .and_then(|bytes| serde_json::from_slice::<BTreeMap<String, String>>(bytes).ok()),
        )
    else {
        return labels;
    };
    if index.documents.len() > 1_000 {
        return labels;
    }
    let files: BTreeMap<_, _> = capture.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let mut declarations: BTreeMap<&str, Vec<&native_evidence::Declaration>> = BTreeMap::new();
    for d in &native.declarations {
        declarations.entry(&d.document.path).or_default().push(d);
    }
    let mut seen = BTreeSet::new();
    for doc in index.documents {
        if !seen.insert(doc.relative_path.clone()) {
            continue;
        }
        let Some(file) = files.get(doc.relative_path.as_str()) else {
            continue;
        };
        // Optional SCIP is a JavaScript presentation hint only; foreign-language metadata
        // never lends authority to Java/Rust/Python native facts.
        if file.language != "javascript"
            || manifest.get(&file.path) != Some(&file.hash)
            || doc.occurrences.len() > 10_000
        {
            continue;
        }
        let encoding = doc.position_encoding.value();
        if !matches!(encoding, 0..=3) {
            continue;
        }
        let measured = declarations
            .get(file.path.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if measured.len() > 1_000 {
            continue;
        }
        let mut ranges: BTreeMap<Vec<i32>, Vec<&native_evidence::Declaration>> = BTreeMap::new();
        for d in measured {
            let Some(range) = d.name_range.as_ref() else {
                continue;
            };
            if file.text.get(range.start..range.end) != d.name.as_deref() {
                continue;
            }
            let Some(start) = scip_coordinate(&file.text, range.start, encoding) else {
                continue;
            };
            let Some(end) = scip_coordinate(&file.text, range.end, encoding) else {
                continue;
            };
            let coordinates = if start.0 == end.0 {
                vec![start.0, start.1, end.1]
            } else {
                vec![start.0, start.1, end.0, end.1]
            };
            ranges.entry(coordinates).or_default().push(d);
        }
        let mut matches: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for occurrence in doc.occurrences {
            if occurrence.symbol_roles & 1 == 0
                || occurrence.symbol.is_empty()
                || occurrence.symbol.len() > 256
                || occurrence.symbol.chars().any(char::is_control)
            {
                continue;
            }
            let Some(ds) = ranges.get(&occurrence.range) else {
                continue;
            };
            for d in ds {
                // A symbol's text is never a source name; require it at least spells
                // the exact witnessed name before allowing an unauthenticated label.
                if d.name
                    .as_ref()
                    .is_some_and(|name| occurrence.symbol.contains(name))
                {
                    matches
                        .entry(d.syntax_id.clone())
                        .or_default()
                        .push(occurrence.symbol.clone());
                }
            }
        }
        for (id, candidates) in matches {
            if candidates.len() == 1 {
                labels.insert(id, candidates[0].clone());
            }
        }
    }
    labels
}

fn scip_coordinate(text: &str, byte: usize, encoding: i32) -> Option<(i32, i32)> {
    let prefix = text.get(..byte)?;
    let line = i32::try_from(prefix.bytes().filter(|b| *b == b'\n').count()).ok()?;
    let part = prefix.rsplit('\n').next()?;
    let column = match encoding {
        1 => part.len(),                      // UTF-8
        3 => part.chars().count(),            // UTF-32
        0 | 2 => part.encode_utf16().count(), // SCIP default UTF-16
        _ => return None,
    };
    Some((line, i32::try_from(column).ok()?))
}

fn project_native(
    options: &IndexOptions,
    capture: &Capture,
    native: &native_evidence::Artifact,
    cancel: &CancelFlag,
    progress: &impl Fn(IndexProgress),
) -> Result<Graph> {
    ensure!(!cancel.load(Ordering::Relaxed), "indexing cancelled");
    capture.claim_graph_projection()?;
    let files: BTreeMap<_, _> = capture.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let mut graph = Graph {
        files: capture.files.clone(),
        ..Graph::default()
    };
    let display_labels = measured_display_labels(options, capture, native);
    let mut owners: BTreeMap<(String, String), String> = BTreeMap::new();
    // Parent is the measured #22 ancestor chain, never a name-based match.
    for d in &native.declarations {
        let file = files
            .get(d.document.path.as_str())
            .context("missing graph source")?;
        let key = serde_json::to_string(&d.ancestors)?;
        let parent = if d.ancestors.is_empty() {
            None
        } else {
            let prefix = serde_json::to_string(&d.ancestors[..d.ancestors.len() - 1])?;
            let last = serde_json::to_string(&d.ancestors[d.ancestors.len() - 1])?;
            owners
                .get(&(d.document.path.clone(), format!("{prefix}/{last}")))
                .cloned()
        };
        ensure!(
            d.ancestors.is_empty() || parent.is_some(),
            "native declaration parent missing"
        );
        let current = serde_json::to_string(&d.key)?;
        owners.insert(
            (d.document.path.clone(), format!("{key}/{current}")),
            d.syntax_id.clone(),
        );
        graph.nodes.push(Symbol {
            id: d.syntax_id.clone(),
            name: d.name.clone().unwrap_or_else(|| {
                if d.kind == "module" {
                    file.path.clone()
                } else {
                    format!("<{}@{}>", d.kind, d.range.start)
                }
            }),
            display_label: display_labels.get(&d.syntax_id).cloned(),
            kind: graph_kind(&d.kind)?,
            path: d.document.path.clone(),
            range: measured_range(file, &d.range)?,
            parent,
            accessor: d.header.modifiers.iter().any(|m| m == "get" || m == "set"),
            provenance: graph_provenance(),
        });
    }
    for r in &native.control_regions {
        let file = files
            .get(r.document.path.as_str())
            .context("missing region source")?;
        let text = file
            .text
            .get(r.range.start..r.range.end)
            .context("invalid region text")?;
        graph.regions.push(ControlRegion {
            id: r.id.clone(),
            kind: r.kind.clone(),
            label: text.chars().take(140).collect(),
            parent: r.parent_id.clone(),
            owner: r.owner_syntax_id.clone(),
            path: r.document.path.clone(),
            range: measured_range(file, &r.range)?,
        });
    }
    for c in &native.calls {
        let file = files
            .get(c.document.path.as_str())
            .context("missing call source")?;
        graph.calls.push(CallSite {
            id: c.id.clone(),
            caller: c.owner_syntax_id.clone(),
            callee_text: c.spelling.clone(),
            path: c.document.path.clone(),
            range: measured_range(file, &c.range)?,
            callee_range: c
                .callee_range
                .as_ref()
                .map(|r| measured_range(file, r))
                .transpose()?,
            ordinal: c.ordinal,
            regions: c.region_ids.clone(),
            provenance: graph_provenance(),
        });
    }
    for coverage in &native.coverage {
        if coverage.state != "complete" {
            let recovered = coverage
                .diagnostic
                .as_deref()
                .is_some_and(|message| message.contains("parser recovered"));
            if recovered {
                graph.stats.parse_error_files += 1;
            }
            graph.diagnostics.push(Diagnostic {
                path: Some(coverage.document_path.clone()),
                code: if recovered {
                    "parse-error"
                } else {
                    "native-coverage-partial"
                }
                .into(),
                message: coverage
                    .diagnostic
                    .clone()
                    .unwrap_or_else(|| "Native extraction is incomplete".into()),
            });
        }
    }
    graph.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    graph.calls.sort_by(|a, b| {
        (&a.path, a.range.start_byte, a.range.end_byte).cmp(&(
            &b.path,
            b.range.start_byte,
            b.range.end_byte,
        ))
    });
    graph.regions.sort_by(|a, b| a.id.cmp(&b.id));
    graph.stats.files = graph.files.len();
    graph.stats.symbols = graph.nodes.len();
    graph.stats.calls = graph.calls.len();
    graph.stats.regions = graph.regions.len();
    graph.stats.unresolved = graph.calls.len();
    for i in 0..graph.files.len() {
        ensure!(!cancel.load(Ordering::Relaxed), "indexing cancelled");
        progress(IndexProgress {
            phase: "parse".into(),
            completed: i + 1,
            total: graph.files.len(),
        });
    }
    ensure!(!cancel.load(Ordering::Relaxed), "indexing cancelled");
    progress(IndexProgress {
        phase: "complete".into(),
        completed: graph.files.len(),
        total: graph.files.len(),
    });
    Ok(graph)
}

/// Verify every public graph row against the exact captured native syntax projection.
/// Display labels are presentation-only SCIP text and cannot authorize a graph edge.
pub fn validate_native_graph(
    graph: &Graph,
    capture: &Capture,
    native: &native_evidence::Artifact,
    cancel: &CancelFlag,
) -> Result<()> {
    ensure!(
        graph.files == capture.files,
        "native_evidence_required: graph source differs from capture"
    );
    validate_native_graph_records(graph, native, cancel)
}

/// Cross-witness only the supplied graph rows against native facts. A read may
/// supply one selected document; this never projects a second whole graph.
pub(crate) fn validate_native_graph_records(
    graph: &Graph,
    native: &native_evidence::Artifact,
    cancel: &CancelFlag,
) -> Result<()> {
    let files: BTreeMap<_, _> = graph.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let mut declaration_keys = BTreeMap::new();
    for d in &native.declarations {
        let key = (
            d.document.path.as_str(),
            serde_json::to_string(&d.ancestors)?,
            serde_json::to_string(&d.key)?,
        );
        ensure!(
            declaration_keys.insert(key, d.syntax_id.as_str()).is_none(),
            "native declaration key collision"
        );
    }
    let symbols: BTreeMap<_, _> = graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    ensure!(
        symbols.len() == native.declarations.len() && graph.nodes.len() == symbols.len(),
        "native_evidence_required: graph declaration cardinality"
    );
    for d in &native.declarations {
        ensure!(!cancel.load(Ordering::Relaxed), "indexing cancelled");
        let f = files
            .get(d.document.path.as_str())
            .context("native declaration source missing")?;
        let symbol = symbols
            .get(d.syntax_id.as_str())
            .context("native_evidence_required: missing measured declaration")?;
        let owner = if let Some(last) = d.ancestors.last() {
            let prefix = serde_json::to_string(&d.ancestors[..d.ancestors.len() - 1])?;
            let last = serde_json::to_string(last)?;
            Some(
                *declaration_keys
                    .get(&(d.document.path.as_str(), prefix, last))
                    .context("native declaration parent missing")?,
            )
        } else {
            None
        };
        if let Some(label) = &symbol.display_label {
            ensure!(
                label.len() <= 256 && !label.chars().any(char::is_control),
                "invalid display-only label"
            );
        }
        ensure!(
            symbol.name
                == d.name.clone().unwrap_or_else(|| if d.kind == "module" {
                    f.path.clone()
                } else {
                    format!("<{}@{}>", d.kind, d.range.start)
                })
                && symbol.kind == graph_kind(&d.kind)?
                && symbol.path == d.document.path
                && symbol.range == measured_range(f, &d.range)?
                && symbol.parent.as_deref() == owner
                && symbol.accessor == d.header.modifiers.iter().any(|m| m == "get" || m == "set")
                && symbol.provenance == graph_provenance(),
            "native_evidence_required: graph declaration differs from measured native row"
        );
    }
    let regions: BTreeMap<_, _> = graph.regions.iter().map(|r| (r.id.as_str(), r)).collect();
    ensure!(
        regions.len() == native.control_regions.len() && graph.regions.len() == regions.len(),
        "native_evidence_required: graph region cardinality"
    );
    for r in &native.control_regions {
        ensure!(!cancel.load(Ordering::Relaxed), "indexing cancelled");
        let f = files
            .get(r.document.path.as_str())
            .context("native region source missing")?;
        let region = regions
            .get(r.id.as_str())
            .context("native_evidence_required: missing measured region")?;
        let text = f
            .text
            .get(r.range.start..r.range.end)
            .context("native region text missing")?;
        ensure!(
            region.kind == r.kind
                && region.label == text.chars().take(140).collect::<String>()
                && region.parent == r.parent_id
                && region.owner == r.owner_syntax_id
                && region.path == r.document.path
                && region.range == measured_range(f, &r.range)?,
            "native_evidence_required: graph region differs from measured native row"
        );
    }
    let calls: BTreeMap<_, _> = graph.calls.iter().map(|c| (c.id.as_str(), c)).collect();
    ensure!(
        calls.len() == native.calls.len() && graph.calls.len() == calls.len(),
        "native_evidence_required: graph call cardinality"
    );
    for c in &native.calls {
        ensure!(!cancel.load(Ordering::Relaxed), "indexing cancelled");
        let f = files
            .get(c.document.path.as_str())
            .context("native call source missing")?;
        let call = calls
            .get(c.id.as_str())
            .context("native_evidence_required: missing measured call")?;
        ensure!(
            call.caller == c.owner_syntax_id
                && call.callee_text == c.spelling
                && call.path == c.document.path
                && call.range == measured_range(f, &c.range)?
                && call.callee_range
                    == c.callee_range
                        .as_ref()
                        .map(|range| measured_range(f, range))
                        .transpose()?
                && call.ordinal == c.ordinal
                && call.regions == c.region_ids
                && call.provenance == graph_provenance(),
            "native_evidence_required: graph call differs from measured native row"
        );
    }
    let mut stats = IndexStats {
        files: graph.files.len(),
        symbols: symbols.len(),
        calls: calls.len(),
        regions: regions.len(),
        unresolved: calls.len(),
        ..IndexStats::default()
    };
    let mut diagnostics = Vec::new();
    for c in &native.coverage {
        if c.state != "complete" {
            let recovered = c
                .diagnostic
                .as_deref()
                .is_some_and(|d| d.contains("parser recovered"));
            if recovered {
                stats.parse_error_files += 1;
            }
            diagnostics.push(Diagnostic {
                path: Some(c.document_path.clone()),
                code: if recovered {
                    "parse-error"
                } else {
                    "native-coverage-partial"
                }
                .into(),
                message: c
                    .diagnostic
                    .clone()
                    .unwrap_or_else(|| "Native extraction is incomplete".into()),
            });
        }
    }
    ensure!(
        graph.stats == stats && graph.diagnostics == diagnostics,
        "native_evidence_required: graph diagnostics or statistics differ from native coverage"
    );
    Ok(())
}

/// Closed #22 syntax categories measured by the JavaScript adapter.
pub(crate) fn native_js_kind(n: Node<'_>) -> Option<&'static str> {
    Some(match n.kind() {
        "class_declaration" | "class" => "type",
        "method_definition" => "method",
        "function_declaration"
        | "function_expression"
        | "generator_function_declaration"
        | "generator_function" => "function",
        "arrow_function" => "anonymousFunction",
        "variable_declarator" => "variable",
        "field_definition" => "field",
        "formal_parameter" => "parameter",
        _ => return None,
    })
}
