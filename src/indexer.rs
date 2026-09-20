//! Native lexical JavaScript, Rust, Java and Python extraction. SCIP is optional JS-only evidence.
//!
//! A paired flat hash manifest is trusted user input, not proof that an artifact was
//! generated from those inputs or by any particular SCIP tool/version. Freshness
//! compares discovered sources and supported root config/lock files only. Regions
//! describe lexical guards, not execution order, dispatch, exceptions, or points-to
//! analysis. Invalid JavaScript retains recovered syntax but no semantic evidence.
//!
//! Regions cover if/else, ternary arms, short-circuit RHS, loops (excluding their
//! once-only initializer/iterable expression), try/catch/finally, and switch/cases.
//! Callback bodies have separate owners; computed method keys retain the outer
//! caller. Accessor reads are never treated as calls to a getter. Dynamic property
//! calls and injected parameters remain unresolved unless explicit callable SCIP
//! evidence exists; this module never infers points-to targets.
//!
//! Root hash scope: package.json, tsconfig.json, jsconfig.json, package-lock.json,
//! yarn.lock, pnpm-lock.yaml, bun.lock, bun.lockb, Cargo.toml, Cargo.lock, plus
//! discovered JS/MJS/CJS/Rust/Java/Python files and supported root Maven/Gradle/Python
//! config/lock files below. Non-JavaScript extraction never consumes SCIP evidence.
//! Transitive configs, dependencies, environment, and indexer tool versions are not
//! attested. Aggregate semantic_state measures manifest freshness; recovered files
//! have Unavailable provenance even when that manifest is Fresh.
//!
//! Discovery skips symlinks. Unix reads use O_NOFOLLOW and validate the opened
//! regular file and inode; malicious concurrent replacement of ancestor directories
//! is not a security boundary (there is no capability-scoped filesystem sandbox).
use crate::model::*;
use anyhow::{Context, Result, ensure};
use protobuf::Message;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
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
fn check(cancel: &CancelFlag) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "indexing cancelled");
    Ok(())
}
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn diag(g: &mut Graph, path: Option<String>, code: &str, message: impl Into<String>) {
    g.diagnostics.push(Diagnostic {
        path,
        code: code.into(),
        message: message.into(),
    });
}
// Discovery never follows directory symlinks; reads also reject file symlinks.
fn safe_read(path: &Path, cap: u64) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && meta.len() <= cap,
        "not a regular file or exceeds byte limit: {}",
        path.display()
    );
    use std::io::Read;
    let mut open = fs::OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Do not follow a final-component symlink inserted after metadata inspection.
        // NONBLOCK also prevents a concurrent replacement with a FIFO from hanging.
        open.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = open.open(path)?;
    let opened = file.metadata()?;
    ensure!(
        opened.is_file() && opened.len() <= cap,
        "opened input is not a regular file or exceeds byte limit: {}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            meta.dev() == opened.dev() && meta.ino() == opened.ino(),
            "input changed during open: {}",
            path.display()
        );
    }
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1)).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= cap, "file grew beyond byte limit");
    Ok(bytes)
}
pub fn index_workspace(
    options: &IndexOptions,
    cancel: &CancelFlag,
    progress: impl Fn(IndexProgress) + Sync,
) -> Result<Graph> {
    check(cancel)?;
    ensure!(
        options.workspace_root.is_dir(),
        "workspace root is not a directory"
    );
    ensure!(
        !fs::symlink_metadata(&options.workspace_root)?
            .file_type()
            .is_symlink(),
        "workspace root is a symlink"
    );
    let workspace_root = fs::canonicalize(&options.workspace_root)?;
    let mut g = Graph::default();
    let mut paths = Vec::new();
    let mut walk = ignore::WalkBuilder::new(&workspace_root);
    let filter_root = workspace_root.clone();
    walk.require_git(false)
        .follow_links(false)
        .hidden(true)
        .filter_entry(move |e| {
            if e.depth() == 0 {
                return true;
            }
            match e.file_name().to_str() {
                Some(".git" | "node_modules" | ".venv" | ".baleyg") => false,
                Some("target" | "dist" | "build") => {
                    // These are legal Java package names inside conventional source
                    // trees, not build-output roots. Earlier artifact ancestors and
                    // .gitignore rules still exclude generated/dependency trees.
                    e.path()
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
                        })
                }
                _ => true,
            }
        });
    for entry in walk.build() {
        check(cancel)?;
        match entry {
            Ok(e)
                if e.file_type().is_some_and(|t| t.is_file())
                    && matches!(
                        e.path().extension().and_then(|x| x.to_str()),
                        Some("js" | "mjs" | "cjs" | "rs" | "java" | "py")
                    ) =>
            {
                paths.push(e.into_path())
            }
            Err(e) => diag(&mut g, None, "scan-error", e.to_string()),
            _ => {}
        }
        ensure!(
            paths.len() <= 100_000,
            "workspace exceeds 100000 source files"
        );
    }
    paths.sort();
    let total = paths.len();
    let mut bytes_total = 0usize;
    let mut hashes = BTreeMap::new();
    for (i, path) in paths.into_iter().enumerate() {
        check(cancel)?;
        let rel = path
            .strip_prefix(&workspace_root)?
            .to_str()
            .context("non-UTF8 source path")?
            .replace('\\', "/");
        match safe_read(&path, options.max_file_bytes.min(256 * 1024 * 1024))
            .and_then(|b| Ok((digest(&b), String::from_utf8(b)?)))
        {
            Ok((hash, text)) => {
                bytes_total += text.len();
                ensure!(
                    bytes_total <= 256 * 1024 * 1024,
                    "workspace source exceeds 256 MiB"
                );
                hashes.insert(rel.clone(), hash.clone());
                g.files.push(SourceFile {
                    path: rel,
                    hash,
                    language: match path.extension().and_then(|ext| ext.to_str()) {
                        Some("rs") => "rust",
                        Some("java") => "java",
                        Some("py") => "python",
                        _ => "javascript",
                    }
                    .into(),
                    text,
                });
            }
            Err(e) => diag(&mut g, Some(rel), "source-skipped", e.to_string()),
        }
        progress(IndexProgress {
            phase: "scan".into(),
            completed: i + 1,
            total,
        });
    }
    for config in [
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
    ] {
        let p = workspace_root.join(config);
        if fs::symlink_metadata(&p).is_ok() {
            match safe_read(&p, options.max_file_bytes.min(16 * 1024 * 1024)) {
                Ok(b) => {
                    hashes.insert(config.into(), digest(&b));
                }
                Err(e) => diag(&mut g, Some(config.into()), "config-skipped", e.to_string()),
            }
        }
    }
    check(cancel)?;
    let mut documents = BTreeMap::new();
    let mut semantic = SemanticState::Unavailable;
    if let Some(path) = &options.scip_path {
        match safe_read(path, 256 * 1024 * 1024)
            .and_then(|b| Ok(scip::types::Index::parse_from_bytes(&b)?))
        {
            Ok(index) => {
                if let Some(manifest) = &options.manifest_path {
                    match safe_read(manifest, 16 * 1024 * 1024)
                        .and_then(|b| Ok(serde_json::from_slice::<BTreeMap<String, String>>(&b)?))
                    {
                        Ok(prior) => {
                            let keys: BTreeSet<_> =
                                prior.keys().chain(hashes.keys()).cloned().collect();
                            g.stats.changed_files = keys
                                .into_iter()
                                .filter(|p| prior.get(p) != hashes.get(p))
                                .collect();
                            semantic =
                                if g.stats.changed_files.is_empty() && g.diagnostics.is_empty() {
                                    SemanticState::Fresh
                                } else {
                                    SemanticState::Stale
                                };
                        }
                        Err(e) => diag(&mut g, None, "manifest-unavailable", e.to_string()),
                    }
                } else {
                    diag(
                        &mut g,
                        None,
                        "manifest-unavailable",
                        "SCIP requires a matching source hash manifest",
                    );
                }
                if semantic == SemanticState::Fresh {
                    for doc in index.documents {
                        documents.insert(doc.relative_path.clone(), doc);
                    }
                }
            }
            Err(e) => diag(&mut g, None, "scip-unavailable", e.to_string()),
        }
    }
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_javascript::LANGUAGE.into())?;
    for i in 0..g.files.len() {
        check(cancel)?;
        // Temporarily move the text, rather than duplicate every source buffer.
        let file = g.files[i].clone();
        if file.language != "javascript" {
            match file.language.as_str() {
                "rust" => crate::indexer_rust::extract(&mut g, &file, cancel)?,
                "java" => crate::indexer_java::extract(&mut g, &file, cancel)?,
                "python" => crate::indexer_python::extract(&mut g, &file, cancel)?,
                _ => unreachable!("source discovery returned an unsupported language"),
            }
            progress(IndexProgress {
                phase: "parse".into(),
                completed: i + 1,
                total: g.files.len(),
            });
            continue;
        }
        let mut cancelled = |_: &tree_sitter::ParseState| cancel.load(Ordering::Relaxed);
        let tree = parser.parse_with_options(
            &mut |offset, _| &file.text.as_bytes()[offset..],
            None,
            Some(tree_sitter::ParseOptions::new().progress_callback(&mut cancelled)),
        );
        check(cancel)?;
        let tree = tree.context("JavaScript parser failed")?;
        if tree.root_node().has_error() {
            g.stats.parse_error_files += 1;
            diag(
                &mut g,
                Some(file.path.clone()),
                "parse-error",
                "Tree-sitter recovered from invalid JavaScript; results may be incomplete",
            );
        }
        let mut ex = Extractor {
            g: &mut g,
            file: &file,
            doc: if tree.root_node().has_error() {
                None
            } else {
                documents.get(&file.path)
            },
            semantic: if tree.root_node().has_error() {
                SemanticState::Unavailable
            } else {
                semantic
            },
            functions: HashMap::new(),
            classes: HashMap::new(),
            cancel,
        };
        let module = format!("module:{}", file.path);
        ex.g.nodes.push(Symbol {
            id: module.clone(),
            name: file.path.clone(),
            kind: SymbolKind::Module,
            path: file.path.clone(),
            range: range(tree.root_node()),
            parent: None,
            accessor: false,
            provenance: ex.provenance(false),
        });
        ex.declarations(tree.root_node(), &module, 0)?;
        ex.walk(tree.root_node(), &module, &[], 0)?;
        progress(IndexProgress {
            phase: "parse".into(),
            completed: i + 1,
            total: ex.g.files.len(),
        });
    }
    let by_id: HashMap<_, _> = g
        .nodes
        .iter()
        .map(|n| (n.id.clone(), (n.kind, n.accessor)))
        .collect();
    for c in &mut g.calls {
        check(cancel)?;
        if c.candidate_symbols.len() > 1 {
            c.resolution = Resolution::Ambiguous;
        } else if let Some(id) = c.candidate_symbols.first() {
            if by_id
                .get(id)
                .is_some_and(|(k, a)| *k != SymbolKind::Module && !a)
            {
                c.target = Some(id.clone());
                c.resolution = Resolution::Internal;
            } else if !by_id.contains_key(id) && id.ends_with("().") {
                c.target = Some(id.clone());
                c.resolution = Resolution::External;
            }
        }
        c.callback_arguments.retain(|id| {
            by_id
                .get(id)
                .is_some_and(|(k, a)| *k != SymbolKind::Module && !a)
        });
    }
    g.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    g.regions.sort_by(|a, b| a.id.cmp(&b.id));
    g.calls
        .sort_by(|a, b| (&a.path, a.range.start_byte).cmp(&(&b.path, b.range.start_byte)));
    let mut ordinals = HashMap::new();
    for c in &mut g.calls {
        let n = ordinals.entry(c.caller.clone()).or_insert(0);
        *n += 1;
        c.ordinal = *n;
    }
    g.stats.files = g.files.len();
    g.stats.symbols = g.nodes.len();
    g.stats.calls = g.calls.len();
    g.stats.regions = g.regions.len();
    g.stats.semantic_state = semantic;
    for c in &g.calls {
        match c.resolution {
            Resolution::Internal => g.stats.internal += 1,
            Resolution::External => g.stats.external += 1,
            Resolution::Unresolved => g.stats.unresolved += 1,
            Resolution::Ambiguous => g.stats.ambiguous += 1,
        }
    }
    g.diagnostics
        .sort_by(|a, b| (&a.path, &a.code, &a.message).cmp(&(&b.path, &b.code, &b.message)));
    check(cancel)?;
    progress(IndexProgress {
        phase: "complete".into(),
        completed: g.files.len(),
        total: g.files.len(),
    });
    check(cancel)?;
    Ok(g)
}
fn range(n: Node<'_>) -> SourceRange {
    SourceRange {
        start_byte: n.start_byte(),
        end_byte: n.end_byte(),
        start_line: n.start_position().row + 1,
        start_column: n.start_position().column + 1,
        end_line: n.end_position().row + 1,
        end_column: n.end_position().column + 1,
    }
}
fn children(n: Node<'_>) -> Vec<Node<'_>> {
    let mut c = n.walk();
    n.named_children(&mut c).collect()
}
fn unwrap(mut n: Node<'_>) -> Node<'_> {
    while n.kind() == "parenthesized_expression" && n.named_child_count() == 1 {
        n = n.named_child(0).unwrap();
    }
    n
}
fn is_function(n: Node<'_>) -> bool {
    matches!(
        n.kind(),
        "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "generator_function_declaration"
            | "generator_function"
            | "method_definition"
    )
}
struct Extractor<'a> {
    g: &'a mut Graph,
    file: &'a SourceFile,
    doc: Option<&'a scip::types::Document>,
    semantic: SemanticState,
    functions: HashMap<usize, String>,
    classes: HashMap<usize, String>,
    cancel: &'a CancelFlag,
}
impl Extractor<'_> {
    fn text(&self, n: Node<'_>) -> &str {
        &self.file.text[n.byte_range()]
    }
    fn provenance(&self, scip: bool) -> Provenance {
        Provenance {
            source: if scip {
                "scip+tree-sitter"
            } else {
                "tree-sitter"
            }
            .into(),
            semantic: self.semantic,
        }
    }
    fn symbols(&self, n: Option<Node<'_>>, definition: bool) -> Vec<String> {
        let (Some(n), Some(doc)) = (n, self.doc) else {
            return vec![];
        };
        let coord = |byte: usize| {
            let prefix = &self.file.text[..byte];
            let start = prefix.rfind('\n').map_or(0, |i| i + 1);
            let line = prefix.bytes().filter(|b| *b == b'\n').count() as i32;
            let s = &self.file.text[start..byte];
            let col = match doc.position_encoding.value() {
                1 => s.len(),
                3 => s.chars().count(),
                _ => s.encode_utf16().count(),
            };
            (line, col as i32)
        };
        let (sr, sc) = coord(n.start_byte());
        let (er, ec) = coord(n.end_byte());
        doc.occurrences
            .iter()
            .filter(|o| {
                (o.symbol_roles & 1 != 0) == definition
                    && !o.symbol.is_empty()
                    && match o.range.as_slice() {
                        [a, b, c] => *a == sr && *b == sc && sr == er && *c == ec,
                        [a, b, c, d] => [*a, *b, *c, *d] == [sr, sc, er, ec],
                        _ => false,
                    }
            })
            .map(|o| {
                if o.symbol.starts_with("local ") {
                    format!("local:{}:{}:{}", self.file.path, self.file.hash, o.symbol)
                } else {
                    o.symbol.clone()
                }
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    fn declarations(&mut self, n: Node<'_>, parent: &str, depth: usize) -> Result<()> {
        check(self.cancel)?;
        ensure!(depth < 512, "JavaScript nesting exceeds 512 levels");
        let mut owner = parent.to_owned();
        if is_function(n) || matches!(n.kind(), "class_declaration" | "class") {
            let mut name = n.child_by_field_name("name");
            if name.is_none()
                && let Some(p) = n.parent()
            {
                name = match p.kind() {
                    "variable_declarator" => p.child_by_field_name("name"),
                    "pair" => p.child_by_field_name("key"),
                    _ => None,
                };
            }
            let ids = self.symbols(name, true);
            let semantic = ids.len() == 1 && !self.g.nodes.iter().any(|node| node.id == ids[0]);
            let id = if semantic {
                ids[0].clone()
            } else {
                format!(
                    "syntax:{}:{}:{}:{}",
                    self.file.path,
                    self.file.hash,
                    n.start_byte(),
                    n.kind()
                )
            };
            let label = name.map(|v| self.text(v).to_owned()).unwrap_or_else(|| {
                format!(
                    "<callback@{}:{}>",
                    n.start_position().row + 1,
                    n.start_position().column + 1
                )
            });
            let accessor = n.kind() == "method_definition" && {
                let mut cur = n.walk();
                n.children(&mut cur)
                    .any(|c| matches!(c.kind(), "get" | "set"))
            };
            let kind = if n.kind() == "method_definition" {
                SymbolKind::Method
            } else if is_function(n) {
                SymbolKind::Function
            } else {
                SymbolKind::Class
            };
            self.g.nodes.push(Symbol {
                id: id.clone(),
                name: label,
                kind,
                path: self.file.path.clone(),
                range: range(n),
                parent: Some(parent.into()),
                accessor,
                provenance: self.provenance(semantic),
            });
            if is_function(n) {
                self.functions.insert(n.id(), id.clone());
            } else {
                self.classes.insert(n.id(), id.clone());
            }
            owner = id;
        }
        for child in children(n) {
            self.declarations(child, &owner, depth + 1)?;
        }
        Ok(())
    }
    fn region(&mut self, n: Node<'_>, kind: &str, context: &[String], owner: &str) -> Vec<String> {
        let id = format!(
            "region:{}:{}:{}:{}",
            self.file.path,
            self.file.hash,
            n.start_byte(),
            kind
        );
        self.g.regions.push(ControlRegion {
            id: id.clone(),
            kind: kind.into(),
            label: self.text(n).chars().take(140).collect(),
            parent: context.last().cloned(),
            owner: owner.into(),
            path: self.file.path.clone(),
            range: range(n),
        });
        let mut result = context.to_vec();
        result.push(id);
        result
    }
    fn walk(&mut self, n: Node<'_>, caller: &str, context: &[String], depth: usize) -> Result<()> {
        check(self.cancel)?;
        ensure!(depth < 512, "JavaScript nesting exceeds 512 levels");
        let mut caller = caller.to_owned();
        let mut context = context.to_vec();
        let mut skip = None;
        if let Some(id) = self.functions.get(&n.id()).cloned() {
            if n.kind() == "method_definition" {
                skip = n.child_by_field_name("name");
                if let Some(key) = skip {
                    self.walk(key, &caller, &context, depth + 1)?;
                }
            }
            caller = id;
            context.clear();
        }
        // Computed field keys and static initializers run at class definition.
        // Instance values run later, during construction: retain that evidence on
        // the class with a boundary region, never as a call from the enclosing fn.
        if n.kind() == "field_definition" {
            if let Some(key) = n.child_by_field_name("property") {
                self.walk(key, &caller, &context, depth + 1)?;
            }
            if let Some(value) = n.child_by_field_name("value") {
                let mut cursor = n.walk();
                let is_static = n.children(&mut cursor).any(|c| c.kind() == "static");
                if is_static {
                    self.walk(value, &caller, &context, depth + 1)?;
                } else if let Some(owner) = n
                    .parent()
                    .and_then(|body| body.parent())
                    .and_then(|class| self.classes.get(&class.id()))
                    .cloned()
                {
                    let region = self.region(value, "instance-initializer", &[], &owner);
                    self.walk(value, &owner, &region, depth + 1)?;
                    diag(
                        self.g,
                        Some(self.file.path.clone()),
                        "instance-initializer-boundary",
                        format!(
                            "Instance field at line {} is owned by the class; construction timing is not modeled",
                            n.start_position().row + 1
                        ),
                    );
                } else {
                    diag(
                        self.g,
                        Some(self.file.path.clone()),
                        "unsupported-field-owner",
                        "Instance field has no class owner; initializer omitted rather than attributed to enclosing code",
                    );
                }
            }
            return Ok(());
        }
        if matches!(n.kind(), "if_statement" | "ternary_expression") {
            if let Some(c) = n.child_by_field_name("condition") {
                self.walk(c, &caller, &context, depth + 1)?;
            }
            for (field, kind) in if n.kind() == "if_statement" {
                [("consequence", "if"), ("alternative", "else")]
            } else {
                [
                    ("consequence", "conditional-true"),
                    ("alternative", "conditional-false"),
                ]
            } {
                if let Some(c) = n.child_by_field_name(field) {
                    let region = self.region(c, kind, &context, &caller);
                    self.walk(c, &caller, &region, depth + 1)?;
                }
            }
            return Ok(());
        }
        if n.kind() == "binary_expression"
            && n.child_by_field_name("operator")
                .is_some_and(|o| matches!(self.text(o), "&&" | "||" | "??"))
        {
            if let Some(c) = n.child_by_field_name("left") {
                self.walk(c, &caller, &context, depth + 1)?;
            }
            if let Some(c) = n.child_by_field_name("right") {
                let r = self.region(c, "short-circuit", &context, &caller);
                self.walk(c, &caller, &r, depth + 1)?;
            }
            return Ok(());
        }
        if matches!(
            n.kind(),
            "for_statement" | "for_in_statement" | "while_statement" | "do_statement"
        ) {
            skip = n.child_by_field_name(if n.kind() == "for_in_statement" {
                "right"
            } else {
                "initializer"
            });
            if let Some(c) = skip {
                self.walk(c, &caller, &context, depth + 1)?;
            }
            context = self.region(n, "loop", &context, &caller);
        }
        if matches!(
            n.kind(),
            "try_statement"
                | "catch_clause"
                | "finally_clause"
                | "switch_statement"
                | "switch_case"
                | "switch_default"
        ) {
            context = self.region(n, n.kind(), &context, &caller);
        }
        if matches!(n.kind(), "call_expression" | "new_expression") {
            let callee = n
                .child_by_field_name(if n.kind() == "new_expression" {
                    "constructor"
                } else {
                    "function"
                })
                .map(unwrap);
            let token = callee.and_then(|c| {
                if c.kind() == "member_expression" {
                    c.child_by_field_name("property")
                } else {
                    Some(c)
                }
            });
            let candidates = self.symbols(token, false);
            if callee.is_some_and(|c| {
                c.kind() == "subscript_expression" || matches!(self.text(c), "eval" | "import")
            }) {
                diag(
                    self.g,
                    Some(self.file.path.clone()),
                    "dynamic-call",
                    format!(
                        "Dynamic call at line {} is a lexical site, not runtime target inference",
                        n.start_position().row + 1
                    ),
                );
            }
            let mut callbacks = Vec::new();
            if let Some(args) = n.child_by_field_name("arguments") {
                for arg in children(args).into_iter().map(unwrap) {
                    if let Some(id) = self.functions.get(&arg.id()) {
                        callbacks.push(id.clone());
                    } else if matches!(arg.kind(), "identifier" | "member_expression") {
                        callbacks.extend(self.symbols(
                            if arg.kind() == "member_expression" {
                                arg.child_by_field_name("property")
                            } else {
                                Some(arg)
                            },
                            false,
                        ));
                    }
                }
            }
            self.g.calls.push(CallSite {
                id: format!(
                    "call:{}:{}:{}:{}",
                    self.file.path,
                    self.file.hash,
                    n.start_byte(),
                    n.end_byte()
                ),
                caller: caller.clone(),
                callee_text: callee
                    .map(|v| self.text(v).to_owned())
                    .unwrap_or_else(|| "<unknown>".into()),
                path: self.file.path.clone(),
                range: range(n),
                target: None,
                candidate_symbols: candidates,
                resolution: Resolution::Unresolved,
                ordinal: 0,
                regions: context.clone(),
                callback_arguments: callbacks,
                provenance: self.provenance(self.doc.is_some()),
            });
        }
        for child in children(n) {
            if skip.is_none_or(|s| s.id() != child.id()) {
                self.walk(child, &caller, &context, depth + 1)?;
            }
        }
        Ok(())
    }
}
