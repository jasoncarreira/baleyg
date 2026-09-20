//! Bounded Cargo discovery without Cargo execution, network, or archive extraction.
use crate::{
    dependencies::*,
    file_tree::{SourceDir, valid_path},
};
use anyhow::{Result, bail};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use toml::Value;

const MAX_PACKAGES: usize = 1024;
const MAX_FILES: usize = 2000;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_SYMBOLS: usize = 50_000;
// Avoid letting a few large libraries consume the entire global declaration budget.
const MAX_PACKAGE_SYMBOLS: usize = 2500;
const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";
const PARSER_VERSION: &str = "rust-declarations-v1";

fn hash(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for part in parts {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part);
    }
    hex::encode(h.finalize())
}
fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!("dependency catalog cancelled");
    }
    Ok(())
}
fn join(base: &str, path: &str) -> String {
    if base.is_empty() {
        path.into()
    } else {
        format!("{base}/{path}")
    }
}
fn string<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
struct MetadataBudget<'a> {
    fingerprints: Vec<String>,
    bytes: usize,
    reads: usize,
    exhausted: bool,
    cancel: &'a AtomicBool,
}
fn read_toml(root: &SourceDir, path: &str, budget: &mut MetadataBudget<'_>) -> Result<Value> {
    check(budget.cancel)?;
    if budget.exhausted || budget.reads >= 8192 {
        budget.exhausted = true;
        bail!("metadata discovery budget reached");
    }
    budget.reads += 1;
    let text = root.read_file(path)?;
    if budget.bytes + text.len() > 16 * 1024 * 1024 {
        budget.exhausted = true;
        bail!("metadata byte budget reached");
    }
    budget.bytes += text.len();
    budget.fingerprints.push(hash(&[
        root.root.as_os_str().as_encoded_bytes(),
        path.as_bytes(),
        text.as_bytes(),
    ]));
    Ok(toml::from_str(&text)?)
}
fn package(name: &str, version: &str, source: &str) -> Package {
    Package {
        id: format!(
            "package:{}",
            hash(&[
                b"cargo",
                name.as_bytes(),
                version.as_bytes(),
                source.as_bytes()
            ])
        ),
        ecosystem: "cargo".into(),
        name: name.into(),
        version: version.into(),
        source: source.into(),
        aliases: vec![],
        source_state: "missing".into(),
        index_state: "skipped".into(),
        warnings: vec![],
    }
}
struct Candidate {
    package: Package,
    directory: Option<Arc<SourceDir>>,
    base: String,
    crate_name: String,
    priority: u8,
}
#[derive(Clone)]
struct Direct {
    name: String,
    alias: String,
    path: Option<String>,
    blocked: bool,
}
fn direct_dependencies(manifest: &Value) -> (Vec<Direct>, bool) {
    let mut tables = vec![manifest];
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        tables.extend(targets.values());
    }
    let mut out = vec![];
    let mut truncated = false;
    for table in tables {
        for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
            if let Some(deps) = table.get(section).and_then(Value::as_table) {
                for (alias, spec) in deps {
                    let name = spec.get("package").and_then(Value::as_str).unwrap_or(alias);
                    let path = spec.get("path").and_then(Value::as_str);
                    if alias.len() > 128
                        || name.len() > 128
                        || path.is_some_and(|p| p.len() > 8192)
                        || out.len() >= MAX_PACKAGES
                    {
                        truncated = true;
                        continue;
                    }
                    out.push(Direct {
                        name: name.to_string(),
                        alias: alias.clone(),
                        path: path.map(str::to_string),
                        blocked: spec.get("git").is_some()
                            || spec.get("registry").is_some()
                            || spec.get("workspace").is_some(),
                    });
                }
            }
        }
    }
    (out, truncated)
}
fn configured(root: &SourceDir, prefix: &str) -> bool {
    // Any project configuration is unsupported, even unreadable/symlink configuration.
    match root.list(prefix, 0, 200) {
        Ok((entries, next, truncated)) => {
            next.is_some()
                || truncated
                || entries
                    .iter()
                    .any(|e| matches!(e.name.as_str(), "config" | "config.toml"))
        }
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}
fn partial(package: &mut Package, warning: &str) {
    package.index_state = "partial".into();
    if package.warnings.len() < 32 && !package.warnings.iter().any(|w| w == warning) {
        package.warnings.push(warning.into());
    }
}

pub(crate) fn build(
    workspace: &Path,
    revision: u64,
    options: &CatalogOptions,
    cancel: &AtomicBool,
) -> Result<Catalog> {
    check(cancel)?;
    let workspace = Arc::new(SourceDir::open(workspace)?);
    let mut fingerprints = MetadataBudget {
        fingerprints: vec![],
        bytes: 0,
        reads: 0,
        exhausted: false,
        cancel,
    };
    let mut catalog = Catalog { id:String::new(), workspace_revision:revision, packages:vec![], symbols:vec![], warnings:vec![
        "Local syntax candidates only; active features, targets, cfg, exports and compiler resolution are unknown. Sources never enter workspace graphs or model context.".into(),
        "Catalog is in-memory and rebuilt on startup or explicit refresh; local cache registry identity is not attested.".into(),
        "Dependency configuration is captured at catalog refresh, not attested to cached workspace source; use Index after changing manifests or toolchains.".into()], sources:HashMap::new() };
    let manifest = match read_toml(&workspace, "Cargo.toml", &mut fingerprints) {
        Ok(value) => value,
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            catalog.warnings.push(
                "No Cargo.toml; automatic dependency discovery currently supports Rust/Cargo only."
                    .into(),
            );
            catalog.id = format!(
                "catalog:{}",
                hash(&[
                    PARSER_VERSION.as_bytes(),
                    &revision.to_be_bytes(),
                    b"no-cargo"
                ])
            );
            return Ok(catalog);
        }
        Err(e) => return Err(e.context("cannot securely read Cargo.toml")),
    };
    if manifest.get("workspace").is_some() {
        catalog.warnings.push("Workspace member manifests and inherited dependencies are not resolved in this slice; lock packages remain local candidates.".into());
    }
    let (direct, direct_truncated) = direct_dependencies(&manifest);
    if direct_truncated {
        catalog.warnings.push(
            "Partial catalog: direct dependency count or name/alias/path text bound reached."
                .into(),
        );
    }
    let cargo_home = options
        .cargo_home
        .as_ref()
        .and_then(|path| match SourceDir::open(path) {
            Ok(root) => Some(Arc::new(root)),
            Err(_) => {
                catalog
                    .warnings
                    .push("Configured Cargo home is unavailable.".into());
                None
            }
        });
    let blocked_config = manifest.get("patch").is_some()
        || manifest.get("replace").is_some()
        || configured(&workspace, ".cargo")
        || cargo_home.as_ref().is_some_and(|root| configured(root, ""));
    if blocked_config {
        catalog.warnings.push("Unsupported Cargo configuration, patch or source replacement: registry candidates blocked.".into());
    }
    catalog.warnings.push("Cargo environment and ancestor configuration are not executed or resolved; cache origins are unverified candidates.".into());
    let mut candidates = vec![];
    if let Some(path) = &options.rust_library {
        match SourceDir::open(path) {
            Ok(root) => {
                let root = Arc::new(root);
                for name in ["std", "core", "alloc"] {
                    let mut p = package(name, "toolchain", "stdlib");
                    p.aliases.push(name.into());
                    p.warnings.push("Configured standard-library syntax candidate; toolchain/workspace compatibility is unverified.".into());
                    let present = root.read_file(&format!("{name}/src/lib.rs")).is_ok();
                    p.source_state = if present { "present" } else { "missing" }.into();
                    candidates.push(Candidate {
                        package: p,
                        directory: present.then(|| root.clone()),
                        base: name.into(),
                        crate_name: name.into(),
                        priority: 0,
                    });
                }
            }
            Err(_) => catalog
                .warnings
                .push("Configured Rust library root is unavailable.".into()),
        }
    } else {
        catalog.warnings.push(
            "Rust standard-library sources unavailable: no trusted rust_library configured.".into(),
        );
    }
    let lock = match read_toml(&workspace, "Cargo.lock", &mut fingerprints) {
        Ok(value) => Some(value),
        Err(_) => {
            catalog.warnings.push("Cargo.lock missing or unreadable; registry versions cannot be resolved without executing Cargo.".into());
            None
        }
    };
    let root_name = manifest
        .get("package")
        .map(|v| string(v, "name"))
        .unwrap_or("");
    let mut registry_dirs = vec![];
    if let Some(home) = &cargo_home
        && let Ok((dirs, next, truncated)) = home.list("registry/src", 0, 200)
    {
        if next.is_some() || truncated {
            catalog.warnings.push(
                "Registry directory discovery truncated; candidates blocked as ambiguous.".into(),
            );
        } else {
            registry_dirs = dirs
                .into_iter()
                .filter(|e| e.kind == "directory")
                .map(|e| e.path)
                .collect();
        }
    }
    if let Some(packages) = lock
        .as_ref()
        .and_then(|v| v.get("package"))
        .and_then(Value::as_array)
    {
        for entry in packages {
            check(cancel)?;
            if candidates.len() >= MAX_PACKAGES {
                catalog
                    .warnings
                    .push("Partial catalog: package discovery limit reached (1024).".into());
                break;
            }
            let name = string(entry, "name");
            let version = string(entry, "version");
            let source = string(entry, "source");
            if name.len() > 128 || version.len() > 128 || source.len() > 8192 {
                if !catalog
                    .warnings
                    .iter()
                    .any(|w| w.contains("package identity text bound"))
                {
                    catalog.warnings.push("Partial catalog: package identity text bound reached; oversized identity omitted.".into());
                }
                continue;
            }
            if source.is_empty()
                && (name == root_name || direct.iter().any(|d| d.name == name && d.path.is_some()))
            {
                continue;
            }
            let mut p = package(
                name,
                version,
                if source.is_empty() {
                    "unresolved-workspace"
                } else {
                    source
                },
            );
            for dep in direct.iter().filter(|d| d.name == name && d.path.is_none()) {
                if p.aliases.len() >= 64 {
                    p.warnings
                        .push("Partial metadata: package alias limit reached (64).".into());
                    break;
                }
                p.aliases.push(dep.alias.replace('-', "_"));
            }
            p.aliases.sort();
            p.aliases.dedup();
            let priority = if p.aliases.is_empty() { 2 } else { 1 };
            let mut directory = None;
            let mut base = String::new();
            let mut crate_name = name.replace('-', "_");
            if source != CRATES_IO
                || blocked_config
                || direct.iter().any(|d| d.name == name && d.blocked)
            {
                p.source_state = "blocked".into();
                p.warnings.push("Unsupported Cargo source/configuration or unresolved workspace package; no files read.".into());
            } else if !valid_path(&format!("{name}-{version}"))
                || name.contains('/')
                || version.contains('/')
                || name.is_empty()
                || version.is_empty()
            {
                p.source_state = "blocked".into();
                p.warnings.push("Invalid package name/version path.".into());
            } else if let Some(home) = &cargo_home {
                let mut matches = vec![];
                for registry in &registry_dirs {
                    check(cancel)?;
                    if fingerprints.exhausted {
                        break;
                    }
                    let candidate = join(registry, &format!("{name}-{version}"));
                    if let Ok(m) =
                        read_toml(home, &join(&candidate, "Cargo.toml"), &mut fingerprints)
                        && m.get("package").is_some_and(|v| {
                            string(v, "name") == name && string(v, "version") == version
                        })
                    {
                        matches.push((candidate, m));
                        if matches.len() == 2 {
                            break;
                        }
                    }
                }
                if fingerprints.exhausted {
                    matches.clear();
                    p.source_state = "ambiguous".into();
                    p.warnings.push("Partial metadata: discovery budget reached; remaining cache directories unsearched, no source selected.".into());
                }
                match matches.len() {
                    0 => p.warnings.push(
                        "Exact locked package not available in the local registry cache.".into(),
                    ),
                    1 => {
                        let (path, m) = matches.pop().unwrap();
                        base = path;
                        directory = Some(home.clone());
                        p.source_state = "present".into();
                        if let Some(name) = m
                            .get("lib")
                            .and_then(|v| v.get("name"))
                            .and_then(Value::as_str)
                        {
                            if name.len() <= 128 {
                                crate_name = name.into();
                            } else {
                                p.warnings
                                    .push("Partial metadata: oversized lib name ignored.".into());
                            }
                        }
                        p.warnings.push("Local cache name/version match only; registry origin identity and package checksum are not verified.".into());
                    }
                    _ => {
                        p.source_state = "ambiguous".into();
                        p.warnings
                            .push("Multiple local cache matches; no source selected.".into());
                    }
                }
            } else {
                p.warnings
                    .push("No readable trusted Cargo home configured.".into());
            }
            p.warnings
                .push("Normal/dev/build/target and optional dependency activation unknown.".into());
            candidates.push(Candidate {
                package: p,
                directory,
                base,
                crate_name,
                priority,
            });
        }
    }
    let mut seen_paths = HashSet::new();
    for dep in &direct {
        check(cancel)?;
        if candidates.len() >= MAX_PACKAGES {
            catalog
                .warnings
                .push("Partial catalog: package discovery limit reached (1024).".into());
            break;
        }
        let Some(path) = &dep.path else {
            if !candidates.iter().any(|c| c.package.name == dep.name) {
                let mut p = package(&dep.name, "unresolved", "manifest");
                p.aliases.push(dep.alias.replace('-', "_"));
                p.warnings.push(
                    "No locked supported version; registry cache not searched by guess.".into(),
                );
                if dep.blocked {
                    p.source_state = "blocked".into();
                }
                candidates.push(Candidate {
                    crate_name: dep.name.replace('-', "_"),
                    package: p,
                    directory: None,
                    base: String::new(),
                    priority: 1,
                });
            }
            continue;
        };
        // Normalize only benign ./ components, never parent traversal or absolute paths.
        let path = path.trim_start_matches("./");
        if !seen_paths.insert(path.to_string()) {
            continue;
        }
        let path_source = format!("workspace-path:{path}");
        let path_source = if path_source.len() <= 8192 {
            path_source
        } else {
            format!("workspace-path:{}", hash(&[path.as_bytes()]))
        };
        let mut p = package(&dep.name, "unresolved", &path_source);
        p.aliases.push(dep.alias.replace('-', "_"));
        let mut directory = None;
        let mut crate_name = dep.name.replace('-', "_");
        if path.is_empty()
            || !valid_path(path)
            || dep.blocked
            || dep
                .path
                .as_ref()
                .is_some_and(|v| v.starts_with('/') || v.split('/').any(|p| p == ".."))
        {
            p.source_state = "blocked".into();
            p.warnings.push(
                "Path dependency outside the workspace or unsupported path is blocked.".into(),
            );
        } else {
            match read_toml(&workspace, &join(path, "Cargo.toml"), &mut fingerprints) {
                Ok(m)
                    if m.get("package")
                        .is_some_and(|v| string(v, "name") == dep.name) =>
                {
                    let version = m.get("package").map(|v| string(v, "version")).unwrap_or("");
                    let version = if version.len() <= 128 {
                        version
                    } else {
                        "unresolved"
                    };
                    p = package(&dep.name, version, &path_source);
                    p.aliases.push(dep.alias.replace('-', "_"));
                    p.source_state = "present".into();
                    directory = Some(workspace.clone());
                    if let Some(name) = m
                        .get("lib")
                        .and_then(|v| v.get("name"))
                        .and_then(Value::as_str)
                    {
                        if name.len() <= 128 {
                            crate_name = name.into();
                        } else {
                            p.warnings
                                .push("Partial metadata: oversized lib name ignored.".into());
                        }
                    }
                }
                _ => {
                    p.source_state = "blocked".into();
                    p.warnings.push(
                        "Path manifest missing, mismatched, or blocked by secure reader.".into(),
                    );
                }
            }
        }
        candidates.push(Candidate {
            package: p,
            directory,
            base: path.into(),
            crate_name,
            priority: 1,
        });
    }
    candidates.sort_by(|a, b| {
        (
            a.priority,
            a.package.name != "std",
            &a.package.name,
            &a.package.version,
        )
            .cmp(&(
                b.priority,
                b.package.name != "std",
                &b.package.name,
                &b.package.version,
            ))
    });
    let mut total_files = 0;
    let mut total_bytes = 0;
    let mut symbol_bytes = 0;
    if fingerprints.exhausted {
        catalog.warnings.push(
            "Partial catalog: metadata discovery budget reached (8192 reads / 16MiB).".into(),
        );
    }
    for mut candidate in candidates {
        check(cancel)?;
        if let Some(root) = &candidate.directory {
            candidate.package.warnings.push("Only src/**/*.rs is indexed; custom library paths, generated code and nested path dependency manifests are not resolved.".into());
            candidate.package.index_state = "complete".into();
            index_package(
                &mut catalog,
                &mut candidate.package,
                root,
                &candidate.base,
                &candidate.crate_name,
                &mut total_files,
                &mut total_bytes,
                &mut symbol_bytes,
                cancel,
            )?;
        }
        catalog.packages.push(candidate.package);
    }
    if catalog.packages.iter().any(|p| p.index_state == "partial") {
        catalog.warnings.push("Partial catalog: one or more packages reached a bound or contain unsupported/unreadable source.".into());
    }
    fingerprints.fingerprints.sort();
    let sources: BTreeMap<_, _> = catalog.sources.iter().map(|(k, v)| (k, &v.hash)).collect();
    catalog.id = format!(
        "catalog:{}",
        hash(&[
            PARSER_VERSION.as_bytes(),
            &revision.to_be_bytes(),
            &serde_json::to_vec(&catalog.packages)?,
            &serde_json::to_vec(&sources)?,
            &serde_json::to_vec(&fingerprints.fingerprints)?
        ])
    );
    Ok(catalog)
}

#[allow(clippy::too_many_arguments)]
fn index_package(
    catalog: &mut Catalog,
    package: &mut Package,
    root: &Arc<SourceDir>,
    base: &str,
    crate_name: &str,
    total_files: &mut usize,
    total_bytes: &mut usize,
    symbol_bytes: &mut usize,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut dirs = VecDeque::from([join(base, "src")]);
    let mut files = 0;
    let mut bytes = 0;
    let mut visited = 0;
    let mut package_symbols = 0;
    while let Some(dir) = dirs.pop_front() {
        let mut offset = 0;
        loop {
            check(cancel)?;
            if package_symbols >= MAX_PACKAGE_SYMBOLS {
                partial(
                    package,
                    "Partial: package declaration limit reached (2500).",
                );
                return Ok(());
            }
            if *total_files >= MAX_FILES
                || *total_bytes >= MAX_BYTES
                || files >= 500
                || bytes >= 16 * 1024 * 1024
                || catalog.symbols.len() >= MAX_SYMBOLS
                || *symbol_bytes >= MAX_BYTES
                || visited >= 50_000
            {
                partial(
                    package,
                    "Partial: source file/byte/symbol/traversal budget reached.",
                );
                return Ok(());
            }
            let (entries, next, truncated) = match root.list(&dir, offset, 200) {
                Ok(v) => v,
                Err(_) => {
                    partial(
                        package,
                        "Partial: source directory missing or blocked by secure reader.",
                    );
                    break;
                }
            };
            if truncated {
                partial(package, "Partial: source directory listing truncated.");
            }
            for entry in entries {
                check(cancel)?;
                visited += 1;
                if entry.name.starts_with('.')
                    || matches!(
                        entry.name.as_str(),
                        "target" | "tests" | "examples" | "benches"
                    )
                {
                    continue;
                }
                if entry.kind == "directory" {
                    if dirs.len() < 2000 {
                        dirs.push_back(entry.path);
                    } else {
                        partial(package, "Partial: directory queue budget reached.");
                    }
                    continue;
                }
                if entry.kind != "file" {
                    partial(package, "Partial: non-regular source entry not followed.");
                    continue;
                }
                if !entry.name.ends_with(".rs") {
                    continue;
                }
                if *total_files >= MAX_FILES || files >= 500 || catalog.symbols.len() >= MAX_SYMBOLS
                {
                    partial(package, "Partial: source file/symbol budget reached.");
                    return Ok(());
                }
                files += 1;
                *total_files += 1;
                let text = match root.read_file(&entry.path) {
                    Ok(v) => v,
                    Err(_) => {
                        partial(
                            package,
                            "Partial: source file unreadable, non-UTF8, symlink, or over 2MiB.",
                        );
                        continue;
                    }
                };
                if bytes + text.len() > 16 * 1024 * 1024 || *total_bytes + text.len() > MAX_BYTES {
                    partial(package, "Partial: source byte budget reached.");
                    return Ok(());
                }
                bytes += text.len();
                *total_bytes += text.len();
                let digest = hex::encode(Sha256::digest(text.as_bytes()));
                let source_ref = format!(
                    "dependency-source:{}",
                    hash(&[
                        package.id.as_bytes(),
                        root.root.as_os_str().as_encoded_bytes(),
                        entry.path.as_bytes(),
                        digest.as_bytes()
                    ])
                );
                match crate::dependency_rust_symbols::extract(
                    &package.id,
                    crate_name,
                    &entry.path,
                    &text,
                    &source_ref,
                ) {
                    Ok(mut declarations) => {
                        if declarations
                            .warnings
                            .iter()
                            .any(|w| w.to_ascii_lowercase().contains("partial"))
                        {
                            partial(
                                package,
                                "Partial: declaration extractor reported incomplete source.",
                            );
                        }
                        for warning in declarations.warnings {
                            if package.warnings.len() < 32 && !package.warnings.contains(&warning) {
                                package.warnings.push(warning);
                            }
                        }
                        let global_available = MAX_SYMBOLS - catalog.symbols.len();
                        let package_available = MAX_PACKAGE_SYMBOLS - package_symbols;
                        let available = global_available.min(package_available);
                        if declarations.symbols.len() > available {
                            // In the final bounded file, favor terminal type declarations over
                            // functions/methods. This is presentation metadata, never behavior.
                            declarations.symbols.sort_by_key(|symbol| {
                                !matches!(
                                    symbol.kind.as_str(),
                                    "struct" | "enum" | "union" | "trait" | "type" | "module"
                                )
                            });
                            declarations.symbols.truncate(available);
                            partial(
                                package,
                                if package_available <= global_available {
                                    "Partial: package declaration limit reached (2500)."
                                } else {
                                    "Partial: catalog symbol budget reached."
                                },
                            );
                        }
                        for symbol in declarations.symbols {
                            let size = symbol_text_bytes(&symbol);
                            if *symbol_bytes + size > MAX_BYTES {
                                *symbol_bytes = MAX_BYTES;
                                partial(
                                    package,
                                    "Partial: catalog declaration text budget reached (64MiB).",
                                );
                                break;
                            }
                            *symbol_bytes += size;
                            package_symbols += 1;
                            catalog.symbols.push(symbol);
                        }
                        catalog.sources.insert(
                            source_ref,
                            CatalogSource {
                                root: root.root.clone(),
                                directory: root.clone(),
                                path: entry.path,
                                hash: digest,
                                package_id: package.id.clone(),
                            },
                        );
                        if package_symbols >= MAX_PACKAGE_SYMBOLS {
                            partial(
                                package,
                                "Partial: package declaration limit reached (2500).",
                            );
                            return Ok(());
                        }
                        if *symbol_bytes >= MAX_BYTES {
                            return Ok(());
                        }
                    }
                    Err(_) => partial(package, "Partial: Rust declaration extraction failed."),
                }
            }
            if let Some(next) = next {
                offset = next;
            } else {
                break;
            }
        }
    }
    Ok(())
}

fn symbol_text_bytes(symbol: &CatalogSymbol) -> usize {
    [
        &symbol.id,
        &symbol.package_id,
        &symbol.name,
        &symbol.qualified_name,
        &symbol.kind,
        &symbol.signature,
        &symbol.source_ref,
        &symbol.path,
    ]
    .iter()
    .map(|s| s.len())
    .sum::<usize>()
        + symbol.parent.as_ref().map_or(0, String::len)
        + symbol.owner_expression.as_ref().map_or(0, String::len)
        + std::mem::size_of::<CatalogSymbol>()
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    #[test]
    fn declaration_budget_counts_all_owned_text() {
        let symbol = CatalogSymbol {
            id: "a".into(),
            package_id: "b".into(),
            name: "c".into(),
            qualified_name: "d".into(),
            kind: "e".into(),
            parent: Some("f".into()),
            owner_expression: Some("g".into()),
            signature: "h".into(),
            source_ref: "i".into(),
            path: "j".into(),
            range: Default::default(),
        };
        assert_eq!(
            symbol_text_bytes(&symbol),
            10 + std::mem::size_of::<CatalogSymbol>()
        );
    }
    #[test]
    fn exhausted_declaration_budget_marks_package_partial_without_storing_text() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/lib.rs"), "pub struct Example;").unwrap();
        let root = Arc::new(SourceDir::open(temp.path()).unwrap());
        let mut p = package("example", "1", "fixture");
        p.index_state = "complete".into();
        let mut catalog = Catalog {
            id: String::new(),
            workspace_revision: 0,
            packages: vec![],
            symbols: vec![],
            warnings: vec![],
            sources: HashMap::new(),
        };
        let (mut files, mut bytes, mut symbols) = (0, 0, MAX_BYTES - 1);
        index_package(
            &mut catalog,
            &mut p,
            &root,
            "",
            "example",
            &mut files,
            &mut bytes,
            &mut symbols,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(p.index_state, "partial");
        assert!(catalog.symbols.is_empty());
        assert_eq!(symbols, MAX_BYTES);
        assert!(
            p.warnings
                .iter()
                .any(|w| w.contains("declaration text budget"))
        );
    }
}
