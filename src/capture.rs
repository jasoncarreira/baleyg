//! Immutable admission of filesystem inputs for one indexing pass.
use crate::{
    indexer::IndexOptions,
    model::{CancelFlag, IndexProgress, SourceFile},
};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
};

pub const ROOT_INPUTS: &[&str] = &[
    "package.json",
    "tsconfig.json",
    "jsconfig.json",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lock",
    "bun.lockb",
    "Cargo.toml",
    "Cargo.lock",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "gradle.properties",
    "pyproject.toml",
    "requirements.txt",
    "uv.lock",
    "poetry.lock",
    "Pipfile",
    "Pipfile.lock",
    "rust-toolchain",
    "rust-toolchain.toml",
];

#[derive(Clone, Debug, Eq, PartialEq)]
struct Stamp {
    kind: u8,
    len: u64,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    mtime: (i64, i64),
    #[cfg(unix)]
    ctime: (i64, i64),
}
fn stamp(meta: &fs::Metadata) -> Stamp {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Stamp {
        kind: if meta.is_dir() {
            1
        } else if meta.is_file() {
            2
        } else {
            3
        },
        len: meta.len(),
        #[cfg(unix)]
        dev: meta.dev(),
        #[cfg(unix)]
        ino: meta.ino(),
        #[cfg(unix)]
        mtime: (meta.mtime(), meta.mtime_nsec()),
        #[cfg(unix)]
        ctime: (meta.ctime(), meta.ctime_nsec()),
    }
}
fn check(cancel: &CancelFlag) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "indexing cancelled");
    Ok(())
}
fn metadata(path: &Path) -> Result<Option<Stamp>> {
    match fs::symlink_metadata(path) {
        Ok(m) => Ok(Some(stamp(&m))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("input metadata: {}", path.display())),
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SourceOperations {
    pub opens: usize,
    pub complete_reads: usize,
    pub hashes: usize,
}

fn regular_read(
    path: &Path,
    cap: u64,
    expected: &Stamp,
    cancel: &CancelFlag,
    operations: Option<&mut SourceOperations>,
) -> Result<Vec<u8>> {
    ensure!(
        expected.kind == 2 && expected.len <= cap,
        "unsafe or oversized input: {}",
        path.display()
    );
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("opening input: {}", path.display()))?;
    let mut operations = operations;
    if let Some(counts) = operations.as_deref_mut() {
        counts.opens += 1;
    }
    ensure!(
        stamp(&file.metadata()?) == *expected,
        "input drift during open: {}",
        path.display()
    );
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 65536];
    loop {
        check(cancel)?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        ensure!(
            (bytes.len() as u64) + (n as u64) <= cap,
            "input exceeded byte limit: {}",
            path.display()
        );
        bytes.extend_from_slice(&buffer[..n]);
    }
    ensure!(
        bytes.len() as u64 == expected.len && stamp(&file.metadata()?) == *expected,
        "input drift during read: {}",
        path.display()
    );
    ensure!(
        metadata(path)?.as_ref() == Some(expected),
        "input replaced during read: {}",
        path.display()
    );
    if let Some(counts) = operations {
        counts.complete_reads += 1;
    }
    Ok(bytes)
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
#[cfg(unix)]
fn identity(item: &Stamp) -> Option<(u64, u64)> {
    Some((item.dev, item.ino))
}
#[cfg(not(unix))]
fn identity(_item: &Stamp) -> Option<(u64, u64)> {
    None
}

fn admit_input(
    path: PathBuf,
    cap: u64,
    cancel: &CancelFlag,
    source_identities: &BTreeSet<(u64, u64)>,
    identities: &mut BTreeMap<(u64, u64), PathBuf>,
    inputs: &mut BTreeMap<PathBuf, Option<Stamp>>,
    input_bytes: &mut BTreeMap<PathBuf, Option<Arc<[u8]>>>,
) -> Result<()> {
    let state = metadata(&path)?;
    let bytes = match &state {
        None => None,
        Some(item) => {
            ensure!(
                item.kind == 2 && item.len <= cap,
                "unsafe or oversized input: {}",
                path.display()
            );
            let key = identity(item);
            ensure!(
                !key.is_some_and(|key| source_identities.contains(&key)),
                "input aliases source: {}",
                path.display()
            );
            if let Some(old_path) = key.and_then(|key| identities.get(&key)) {
                ensure!(
                    metadata(old_path)? == inputs[old_path],
                    "input alias drift: {}",
                    old_path.display()
                );
                input_bytes
                    .get(old_path)
                    .cloned()
                    .context("missing admitted input")?
            } else {
                Some(Arc::from(regular_read(&path, cap, item, cancel, None)?))
            }
        }
    };
    if let Some(key) = state.as_ref().and_then(identity) {
        identities.entry(key).or_insert_with(|| path.clone());
    }
    inputs.insert(path.clone(), state);
    input_bytes.insert(path, bytes);
    Ok(())
}
fn source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|x| x.to_str()),
        Some("js" | "mjs" | "cjs" | "rs" | "java" | "py")
    )
}
fn walk(root: &Path, cancel: &CancelFlag) -> Result<(BTreeMap<PathBuf, Stamp>, Vec<PathBuf>)> {
    let mut builder = ignore::WalkBuilder::new(root);
    let filter_root = root.to_owned();
    builder
        .require_git(false)
        .follow_links(false)
        .hidden(true)
        .parents(false)
        .git_global(false)
        .git_exclude(false)
        .filter_entry(move |e| {
            if e.depth() == 0 {
                return true;
            }
            match e.file_name().to_str() {
                Some(".git" | "node_modules" | ".venv" | ".baleyg") => false,
                Some("target" | "dist" | "build") => e
                    .path()
                    .strip_prefix(&filter_root)
                    .ok()
                    .is_some_and(|relative| {
                        let parts: Vec<_> = relative.components().collect();
                        parts.windows(3).any(|p| {
                            p[0].as_os_str() == "src"
                                && matches!(
                                    p[1].as_os_str().to_str(),
                                    Some("main" | "test" | "testFixtures")
                                )
                                && p[2].as_os_str() == "java"
                        })
                    }),
                _ => true,
            }
        });
    let mut inventory = BTreeMap::new();
    let mut sources = Vec::new();
    for entry in builder.build() {
        check(cancel)?;
        let entry = entry.context("workspace walk failed")?;
        let path = entry.path();
        let kind = entry.file_type().context("missing entry type")?;
        if kind.is_dir() || source(path) {
            let item = metadata(path)?.context("walk entry disappeared")?;
            if kind.is_dir() {
                ensure!(item.kind == 1, "directory changed: {}", path.display());
            } else {
                ensure!(item.kind == 2, "unsafe source: {}", path.display());
                sources.push(path.to_owned());
            }
            inventory.insert(path.to_owned(), item);
        }
        ensure!(
            sources.len() <= 100_000,
            "workspace exceeds 100000 source files"
        );
    }
    sources.sort();
    Ok((inventory, sources))
}

/// Bytes and complete admission inventory are retained until the final cutoff.
/// No source is physically opened again during validation.
pub struct Capture {
    admission_root: PathBuf,
    root_stamp: Stamp,
    root: PathBuf,
    inventory: BTreeMap<PathBuf, Stamp>,
    inputs: BTreeMap<PathBuf, Option<Stamp>>,
    pub files: Vec<SourceFile>,
    pub source_operations: BTreeMap<String, SourceOperations>,
    pub hashes: BTreeMap<String, String>,
    pub input_bytes: BTreeMap<PathBuf, Option<Arc<[u8]>>>,
}
impl Capture {
    pub fn admit(
        options: &IndexOptions,
        cancel: &CancelFlag,
        progress: &impl Fn(IndexProgress),
    ) -> Result<Self> {
        let exe = std::env::current_exe()?;
        Self::admit_with_executable(options, cancel, progress, exe)
    }
    fn admit_with_executable(
        options: &IndexOptions,
        cancel: &CancelFlag,
        progress: &impl Fn(IndexProgress),
        exe: PathBuf,
    ) -> Result<Self> {
        check(cancel)?;
        let root_meta = metadata(&options.workspace_root)?.context("workspace root absent")?;
        ensure!(
            root_meta.kind == 1,
            "workspace root is not a real directory"
        );
        let root = fs::canonicalize(&options.workspace_root)?;
        let (inventory, sources) = walk(&root, cancel)?;
        let mut inputs = BTreeMap::new();
        let mut input_bytes = BTreeMap::new();
        let mut source_identities = BTreeSet::new();
        let mut input_identities = BTreeMap::new();
        for path in &sources {
            let item = inventory.get(path).context("missing source inventory")?;
            if let Some(key) = identity(item) {
                ensure!(
                    source_identities.insert(key),
                    "duplicate source identity: {}",
                    path.display()
                );
            }
        }
        for name in ROOT_INPUTS {
            admit_input(
                root.join(name),
                options.max_file_bytes.min(16 * 1024 * 1024),
                cancel,
                &source_identities,
                &mut input_identities,
                &mut inputs,
                &mut input_bytes,
            )?;
        }
        for dir in inventory
            .iter()
            .filter(|(_, s)| s.kind == 1)
            .map(|(p, _)| p)
        {
            for name in [".gitignore", ".ignore"] {
                admit_input(
                    dir.join(name),
                    options.max_file_bytes.min(16 * 1024 * 1024),
                    cancel,
                    &source_identities,
                    &mut input_identities,
                    &mut inputs,
                    &mut input_bytes,
                )?;
            }
        }
        // The linked native extractor is part of the running executable, not ambient PATH.
        admit_input(
            exe,
            512 * 1024 * 1024,
            cancel,
            &source_identities,
            &mut input_identities,
            &mut inputs,
            &mut input_bytes,
        )?;
        for path in [options.scip_path.as_ref(), options.manifest_path.as_ref()]
            .into_iter()
            .flatten()
        {
            admit_input(
                path.clone(),
                256 * 1024 * 1024,
                cancel,
                &source_identities,
                &mut input_identities,
                &mut inputs,
                &mut input_bytes,
            )?;
        }
        let mut files = Vec::new();
        let mut hashes = BTreeMap::new();
        let mut source_operations = BTreeMap::new();
        let mut total_bytes = 0u64;
        for (i, path) in sources.iter().enumerate() {
            check(cancel)?;
            let state = inventory
                .get(path)
                .context("source absent from inventory")?;
            let mut counts = SourceOperations::default();
            let bytes = regular_read(
                path,
                options.max_file_bytes.min(256 * 1024 * 1024),
                state,
                cancel,
                Some(&mut counts),
            )?;
            total_bytes += bytes.len() as u64;
            ensure!(
                total_bytes <= 256 * 1024 * 1024,
                "workspace source exceeds 256 MiB"
            );
            let rel = path
                .strip_prefix(&root)?
                .to_str()
                .context("non-UTF8 source path")?
                .replace('\\', "/");
            let text = String::from_utf8(bytes).context("non-UTF8 source text")?;
            let digest = hash(text.as_bytes());
            counts.hashes += 1;
            source_operations.insert(rel.clone(), counts);
            hashes.insert(rel.clone(), digest.clone());
            files.push(SourceFile {
                path: rel,
                hash: digest,
                language: match path.extension().and_then(|e| e.to_str()) {
                    Some("rs") => "rust",
                    Some("java") => "java",
                    Some("py") => "python",
                    _ => "javascript",
                }
                .into(),
                text,
            });
            progress(IndexProgress {
                phase: "scan".into(),
                completed: i + 1,
                total: sources.len(),
            });
        }
        let capture = Self {
            admission_root: options.workspace_root.clone(),
            root_stamp: root_meta,
            root,
            inventory,
            inputs,
            files,
            source_operations,
            hashes,
            input_bytes,
        };
        capture.verify(cancel)?;
        Ok(capture)
    }
    pub fn verify(&self, cancel: &CancelFlag) -> Result<()> {
        check(cancel)?;
        ensure!(
            metadata(&self.admission_root)?.as_ref() == Some(&self.root_stamp),
            "workspace root drift"
        );
        let (inventory, _) = walk(&self.root, cancel)?;
        ensure!(inventory == self.inventory, "workspace inventory drift");
        for (path, expected) in &self.inputs {
            check(cancel)?;
            ensure!(
                &metadata(path)? == expected,
                "input inventory drift: {}",
                path.display()
            );
        }
        check(cancel)?;
        Ok(())
    }
    pub fn bytes(&self, path: &Path) -> Option<&[u8]> {
        self.input_bytes.get(path).and_then(|b| b.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::AtomicBool};
    #[test]
    fn executable_cutoff_and_stable_source_operation_counts() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("fake-native-bin");
        fs::write(&exe, "native-v1").unwrap();
        fs::write(root.path().join("one.js"), "f();").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let capture = Capture::admit_with_executable(
            &IndexOptions::new(root.path().to_owned()),
            &cancel,
            &|_| {},
            exe.clone(),
        )
        .unwrap();
        assert_eq!(
            capture.source_operations["one.js"],
            SourceOperations {
                opens: 1,
                complete_reads: 1,
                hashes: 1,
            }
        );
        capture.verify(&cancel).unwrap();
        fs::write(&exe, "native-v2").unwrap();
        assert!(
            capture
                .verify(&cancel)
                .unwrap_err()
                .to_string()
                .contains("drift")
        );
    }
}
