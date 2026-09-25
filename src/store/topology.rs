//! Verified filesystem identities and lock primitives for the fixed storage topology.
use anyhow::{Context, Result, bail, ensure};
use directories::ProjectDirs;
use sha2::{Digest, Sha256};
use std::os::unix::{
    fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    io::AsRawFd,
};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

fn owner() -> u32 {
    unsafe { libc::geteuid() }
}
fn metadata(path: &Path) -> Result<fs::Metadata> {
    fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))
}
fn private_dir(path: &Path) -> Result<()> {
    let m = metadata(path)?;
    ensure!(
        m.is_dir() && !m.file_type().is_symlink() && m.uid() == owner() && m.mode() & 0o077 == 0,
        "unsafe managed directory: {}",
        path.display()
    );
    Ok(())
}
fn private_file(path: &Path, file: &File) -> Result<()> {
    let m = file.metadata()?;
    let named = metadata(path)?;
    ensure!(
        m.is_file()
            && m.uid() == owner()
            && m.mode() & 0o777 == 0o600
            && m.nlink() == 1
            && named.is_file()
            && !named.file_type().is_symlink()
            && (m.dev(), m.ino()) == (named.dev(), named.ino()),
        "unsafe managed file: {}",
        path.display()
    );
    Ok(())
}
fn open_file(path: &Path, create: bool) -> Result<File> {
    let mut o = OpenOptions::new();
    o.read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    if create {
        o.create(true);
    }
    let f = o.open(path)?;
    private_file(path, &f)?;
    Ok(f)
}
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?
        .sync_all()
        .with_context(|| format!("sync {}", path.display()))
}
fn make_private(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => sync_directory(path.parent().context("directory has no parent")?)?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    private_dir(path)
}
fn managed_tree(path: &Path) -> Result<()> {
    // Existing system ancestors are not ours; each newly created component is private.
    let mut missing = vec![];
    let mut at = path;
    while fs::symlink_metadata(at).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        missing.push(at.to_owned());
        at = at.parent().context("no existing ancestor")?;
    }
    ensure!(
        !metadata(at)?.file_type().is_symlink() && metadata(at)?.is_dir(),
        "unsafe ancestor: {}",
        at.display()
    );
    for p in missing.iter().rev() {
        make_private(p)?;
    }
    private_dir(path)
}

#[derive(Clone, Debug)]
pub struct TopologyRoots {
    pub cache: PathBuf,
    pub data: PathBuf,
}
impl TopologyRoots {
    pub fn production() -> Result<Self> {
        let dirs = ProjectDirs::from("dev", "odin", "baleyg").context("ProjectDirs unavailable")?;
        Ok(Self {
            cache: dirs.cache_dir().to_owned(),
            data: dirs.data_local_dir().to_owned(),
        })
    }
    /// Only integration fixtures may inject isolated roots; production uses `production`.
    pub fn isolated_for_tests(cache: PathBuf, data: PathBuf) -> Self {
        Self { cache, data }
    }
    pub fn index_dir(&self, identity: &WorkspaceIdentity) -> PathBuf {
        self.cache.join("indexes").join(&identity.root_key)
    }
    pub fn index_use_lock(&self, identity: &WorkspaceIdentity) -> PathBuf {
        self.cache
            .join("indexes")
            .join(format!("{}.lock", identity.root_key))
    }
    pub fn index_db(&self, identity: &WorkspaceIdentity) -> PathBuf {
        self.index_dir(identity).join("index.db")
    }
    pub fn leader_lock(&self, identity: &WorkspaceIdentity) -> PathBuf {
        self.index_dir(identity).join("leader.lock")
    }
    pub fn record_dir(&self, identity: &WorkspaceIdentity) -> PathBuf {
        self.data.join("workspaces").join(&identity.record_id)
    }
    pub fn record_use_lock(&self, identity: &WorkspaceIdentity) -> PathBuf {
        self.data
            .join("workspaces")
            .join(format!("{}.lock", identity.record_id))
    }
    pub fn record_db(&self, identity: &WorkspaceIdentity) -> PathBuf {
        self.record_dir(identity).join("workspace.db")
    }
    pub fn reject_root_overlap(&self, identity: &WorkspaceIdentity) -> Result<()> {
        let cache = resolve_existing_ancestor(&self.cache)?;
        let data = resolve_existing_ancestor(&self.data)?;
        ensure!(
            !overlaps(&identity.root, &cache) && !overlaps(&identity.root, &data),
            "workspace root overlaps fixed topology"
        );
        Ok(())
    }
    pub fn prepare_index(&self, identity: &WorkspaceIdentity) -> Result<()> {
        self.reject_root_overlap(identity)?;
        managed_tree(&self.cache)?;
        make_private(&self.cache.join("indexes"))?;
        make_private(&self.index_dir(identity))
    }
    pub fn prepare_records(&self, identity: &WorkspaceIdentity) -> Result<()> {
        self.reject_root_overlap(identity)?;
        managed_tree(&self.data)?;
        make_private(&self.data.join("workspaces"))
    }
    pub fn index_use(&self, identity: &WorkspaceIdentity) -> Result<UseGuard> {
        self.prepare_index(identity)?;
        UseGuard::acquire(&self.index_use_lock(identity), false, false)
    }
    pub fn index_use_existing(&self, identity: &WorkspaceIdentity) -> Result<UseGuard> {
        identity.verify()?;
        self.reject_root_overlap(identity)?;
        for path in [
            &self.cache,
            &self.cache.join("indexes"),
            &self.index_dir(identity),
        ] {
            private_dir(path)?;
        }
        UseGuard::acquire_existing(&self.index_use_lock(identity), false, false)
            .context("incompatible_index: missing or unsafe use lock")
    }
    pub fn record_use(&self, identity: &WorkspaceIdentity, exclusive: bool) -> Result<UseGuard> {
        self.prepare_records(identity)?;
        UseGuard::acquire(&self.record_use_lock(identity), exclusive, exclusive)
    }
    pub fn leader(&self, identity: &WorkspaceIdentity) -> Result<LeaderGuard> {
        self.leader_with_lock_hook(identity, || Ok(()), || Ok(()), || Ok(()))
    }
    /// Fixture hook: pause after locking or fail just before syncing; never used by production callers.
    pub fn leader_with_hooks(
        &self,
        identity: &WorkspaceIdentity,
        before_write: impl FnOnce() -> Result<()>,
        before_sync: impl FnOnce() -> Result<()>,
    ) -> Result<LeaderGuard> {
        self.leader_with_lock_hook(identity, || Ok(()), before_write, before_sync)
    }
    /// Fixture barrier after opening the leader inode, before taking its lock.
    pub fn leader_with_lock_hook(
        &self,
        identity: &WorkspaceIdentity,
        after_open: impl FnOnce() -> Result<()>,
        before_write: impl FnOnce() -> Result<()>,
        before_sync: impl FnOnce() -> Result<()>,
    ) -> Result<LeaderGuard> {
        let use_guard = self.index_use(identity)?;
        let path = self.leader_lock(identity);
        let mut hook = Some(after_open);
        let mut file = None;
        for _ in 0..20 {
            let candidate = open_file(&path, true)?;
            if let Some(after_open) = hook.take() {
                after_open()?;
            }
            let status =
                unsafe { libc::flock(candidate.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if status != 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::WouldBlock {
                    bail!("storage_busy: {}", path.display());
                }
                return Err(e.into());
            }
            if private_file(&path, &candidate).is_ok() {
                file = Some(candidate);
                break;
            }
            // Drop the old descriptor and lock before opening the replacement pathname.
        }
        let mut file = file.context("leader lock pathname changed repeatedly")?;
        before_write()?;
        let incarnation = Uuid::new_v4();
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0))?;
        file.set_len(0)?;
        file.write_all(incarnation.to_string().as_bytes())?;
        before_sync().context("incarnation_not_durable")?;
        file.sync_all().context("incarnation_not_durable")?;
        Ok(LeaderGuard {
            use_guard,
            file,
            path,
            incarnation,
        })
    }
    pub fn validate_external(
        &self,
        identity: &WorkspaceIdentity,
        destinations: &[PathBuf],
    ) -> Result<()> {
        let protected = [
            identity.root.clone(),
            resolve_existing_ancestor(&self.cache)?,
            resolve_existing_ancestor(&self.data)?,
        ];
        let resolved: Vec<PathBuf> = destinations
            .iter()
            .map(|p| resolve_existing_ancestor(p))
            .collect::<Result<_>>()?;
        for target in &resolved {
            ensure!(
                !protected.iter().any(|p| overlaps(target, p)),
                "external destination overlaps checkout or topology: {}",
                target.display()
            );
            for parent in target.ancestors() {
                if parent.join(".git").symlink_metadata().is_ok() {
                    bail!(
                        "external destination inside Git checkout: {}",
                        target.display()
                    );
                }
            }
        }
        for (i, a) in resolved.iter().enumerate() {
            ensure!(
                !resolved[i + 1..].iter().any(|b| overlaps(a, b)),
                "external destinations overlap"
            );
        }
        Ok(())
    }
}
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
fn resolve_existing_ancestor(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "external path must be absolute");
    let mut rest = vec![];
    let mut p = path;
    while fs::symlink_metadata(p).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        rest.push(
            p.file_name()
                .context("invalid external path")?
                .to_os_string(),
        );
        p = p.parent().context("invalid external path")?;
    }
    let mut out = fs::canonicalize(p)?;
    for c in rest.iter().rev() {
        match Path::new(c).components().next() {
            Some(Component::ParentDir) => {
                out.pop();
            }
            Some(Component::CurDir) => {}
            _ => out.push(c),
        }
    }
    Ok(out)
}

#[derive(Debug)]
pub struct WorkspaceIdentity {
    pub root: PathBuf,
    pub root_key: String,
    pub record_id: String,
    pub device: u64,
    pub inode: u64,
    git_dir: Option<PathBuf>,
    marker: Option<Uuid>,
    root_handle: File,
}
impl WorkspaceIdentity {
    pub fn discover(explicit: Option<&Path>, cwd: &Path) -> Result<Self> {
        Self::discover_with_marker_hook(explicit, cwd, |_| Ok(()))
    }
    /// Fixture hook to inject a failure immediately before marker descriptor fsync.
    pub fn discover_with_marker_sync_hook(
        explicit: Option<&Path>,
        cwd: &Path,
        before_sync: impl FnOnce() -> Result<()>,
    ) -> Result<Self> {
        let mut before_sync = Some(before_sync);
        Self::discover_with_marker_hook(explicit, cwd, |stage| {
            if stage == MarkerStage::MarkerSync {
                before_sync.take().context("marker sync hook reused")?()
            } else {
                Ok(())
            }
        })
    }
    /// Fixture-only interleaving and durability hook; ordinary discovery supplies no barrier.
    pub fn discover_with_marker_hook(
        explicit: Option<&Path>,
        cwd: &Path,
        mut hook: impl FnMut(MarkerStage) -> Result<()>,
    ) -> Result<Self> {
        Self::discover_unattached(explicit, cwd)?.attach_marker_with_hook(&mut hook)
    }
    /// Inspect identity without creating the Git marker, for external destination preflight.
    pub fn discover_unattached(explicit: Option<&Path>, cwd: &Path) -> Result<Self> {
        let selected = if let Some(p) = explicit {
            p.to_owned()
        } else {
            let cwd = fs::canonicalize(cwd)?;
            let mut found = None;
            for p in cwd.ancestors() {
                match fs::symlink_metadata(p.join(".git")) {
                    Ok(_) => {
                        found = Some(p.to_owned());
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
            found.unwrap_or(cwd)
        };
        ensure!(
            !metadata(&selected)?.file_type().is_symlink(),
            "root_changed: final root symlink"
        );
        let root = fs::canonicalize(selected)?;
        ensure!(
            root.is_absolute() && root.to_str().is_some() && metadata(&root)?.is_dir(),
            "invalid workspace root"
        );
        if explicit.is_none() {
            ensure!(
                root.parent().is_some() && Some(root.as_path()) != dirs_home().as_deref(),
                "implicit home or filesystem root refused"
            );
        }
        let root_handle = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&root)?;
        let m = root_handle.metadata()?;
        let spelling = root.to_str().context("non-UTF8 workspace")?;
        let spelling = if spelling == "/" {
            spelling
        } else {
            spelling.trim_end_matches('/')
        };
        let root_key = hex::encode(Sha256::digest(spelling.as_bytes()));
        let git_dir = match fs::symlink_metadata(root.join(".git")) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => Some(root.join(".git")),
            Ok(m) if m.is_file() && !m.file_type().is_symlink() => {
                Some(parse_git_pointer(&root.join(".git"))?)
            }
            Ok(_) => bail!("unsafe .git entry"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let record_id = format!("path-{root_key}");
        Ok(Self {
            root,
            root_key,
            record_id,
            device: m.dev(),
            inode: m.ino(),
            git_dir,
            marker: None,
            root_handle,
        })
    }
    pub fn attach_marker(self) -> Result<Self> {
        self.attach_marker_with_hook(&mut |_| Ok(()))
    }
    fn attach_marker_with_hook(
        mut self,
        hook: &mut impl FnMut(MarkerStage) -> Result<()>,
    ) -> Result<Self> {
        let current = metadata(&self.root).context("root_changed")?;
        let captured = self.root_handle.metadata()?;
        ensure!(
            current.is_dir()
                && !current.file_type().is_symlink()
                && (current.dev(), current.ino()) == (self.device, self.inode)
                && (captured.dev(), captured.ino()) == (self.device, self.inode),
            "root_changed"
        );
        if let Some(git) = &self.git_dir {
            ensure!(
                resolve_git_dir(&self.root)?.as_deref() == Some(git),
                "workspace_id_changed"
            );
            let marker = marker_at(git, hook)?;
            self.record_id = marker.to_string();
            self.marker = Some(marker);
        }
        self.verify()?;
        Ok(self)
    }
    pub fn verify(&self) -> Result<()> {
        let m = fs::symlink_metadata(&self.root).context("root_changed")?;
        let handle = self.root_handle.metadata()?;
        ensure!(
            m.is_dir()
                && !m.file_type().is_symlink()
                && (m.dev(), m.ino()) == (self.device, self.inode)
                && (handle.dev(), handle.ino()) == (self.device, self.inode),
            "root_changed"
        );
        if let Some(git) = &self.git_dir {
            ensure!(
                resolve_git_dir(&self.root)?.as_deref() == Some(git),
                "workspace_id_changed"
            );
            ensure!(
                Some(read_marker(&git.join("baleyg/workspace-id"))?) == self.marker,
                "workspace_id_changed"
            );
        }
        Ok(())
    }
    pub fn git_dir(&self) -> Option<&Path> {
        self.git_dir.as_deref()
    }
}
fn dirs_home() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_owned())
}
fn resolve_git_dir(root: &Path) -> Result<Option<PathBuf>> {
    let p = root.join(".git");
    match metadata(&p) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => Ok(Some(p)),
        Ok(m) if m.is_file() && !m.file_type().is_symlink() => Ok(Some(parse_git_pointer(&p)?)),
        Ok(_) => bail!("unsafe .git entry"),
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}
fn parse_git_pointer(path: &Path) -> Result<PathBuf> {
    let m = metadata(path)?;
    ensure!(
        m.uid() == owner() && m.nlink() == 1 && m.len() <= 4096,
        "unsafe .git pointer"
    );
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let opened = file.metadata()?;
    ensure!(
        opened.is_file()
            && opened.uid() == owner()
            && opened.nlink() == 1
            && (opened.dev(), opened.ino()) == (m.dev(), m.ino()),
        "unsafe .git pointer"
    );
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 4096 && !bytes.contains(&0),
        "invalid .git pointer"
    );
    let text = std::str::from_utf8(&bytes)?;
    let value = text
        .strip_prefix("gitdir: ")
        .context("invalid .git pointer")?;
    let value = value
        .strip_suffix("\r\n")
        .or_else(|| value.strip_suffix('\n'))
        .unwrap_or(value);
    ensure!(
        !value.is_empty() && value.trim() == value && !value.contains(['\r', '\n']),
        "invalid .git pointer"
    );
    let target = path.parent().unwrap().join(value);
    let mut normalized = PathBuf::new();
    for component in target.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component)
            }
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
        }
    }
    ensure!(normalized.is_absolute(), "invalid .git pointer target");
    for p in normalized.ancestors().collect::<Vec<_>>().iter().rev() {
        let m = metadata(p)?;
        ensure!(
            !m.file_type().is_symlink(),
            "symlink Git directory traversal"
        );
    }
    let m = metadata(&normalized)?;
    ensure!(m.is_dir() && m.uid() == owner(), "unsafe Git directory");
    Ok(normalized)
}
/// Stages exposed only to integration fixtures; no production behavior depends on a hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerStage {
    CreatedBeforeWrite,
    ShortRead,
    MarkerSync,
    PrivateDirSync,
    GitDirSync,
}

enum MarkerRead {
    Valid(Uuid, File),
    Short(File),
}
fn read_marker_file(path: &Path) -> Result<MarkerRead> {
    let f = open_file(path, false)?;
    let mut data = Vec::new();
    (&f).take(37).read_to_end(&mut data)?;
    private_file(path, &f)?;
    if data.len() < 36 {
        return Ok(MarkerRead::Short(f));
    }
    ensure!(data.len() == 36, "invalid workspace-id marker");
    let text = std::str::from_utf8(&data)?;
    let id = Uuid::parse_str(text)?;
    ensure!(
        !id.is_nil() && id.get_version_num() == 4 && id.to_string() == text,
        "invalid workspace-id marker"
    );
    Ok(MarkerRead::Valid(id, f))
}
fn read_marker(path: &Path) -> Result<Uuid> {
    match read_marker_file(path)? {
        MarkerRead::Valid(id, _) => Ok(id),
        MarkerRead::Short(_) => bail!("invalid workspace-id marker"),
    }
}
fn read_marker_during_creation(
    path: &Path,
    hook: &mut impl FnMut(MarkerStage) -> Result<()>,
) -> Result<(Uuid, File)> {
    for attempt in 0..20 {
        match read_marker_file(path)? {
            MarkerRead::Valid(id, file) => return Ok((id, file)),
            MarkerRead::Short(file) => {
                hook(MarkerStage::ShortRead)?;
                private_file(path, &file)?;
                if attempt == 19 {
                    bail!("invalid workspace-id marker: persistently short");
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    unreachable!()
}
fn durable_marker(
    path: &Path,
    baleyg: &Path,
    git: &Path,
    file: &File,
    hook: &mut impl FnMut(MarkerStage) -> Result<()>,
) -> Result<()> {
    private_file(path, file).context("workspace_id_not_durable")?;
    hook(MarkerStage::MarkerSync).context("workspace_id_not_durable")?;
    file.sync_all().context("workspace_id_not_durable")?;
    private_file(path, file).context("workspace_id_not_durable")?;
    hook(MarkerStage::PrivateDirSync).context("workspace_id_not_durable")?;
    sync_directory(baleyg).context("workspace_id_not_durable")?;
    private_file(path, file).context("workspace_id_not_durable")?;
    hook(MarkerStage::GitDirSync).context("workspace_id_not_durable")?;
    sync_directory(git).context("workspace_id_not_durable")?;
    private_file(path, file).context("workspace_id_not_durable")
}
fn marker_at(git: &Path, hook: &mut impl FnMut(MarkerStage) -> Result<()>) -> Result<Uuid> {
    ensure!(
        metadata(git)?.uid() == owner() && metadata(git)?.is_dir(),
        "unsafe Git directory"
    );
    let baleyg = git.join("baleyg");
    make_private(&baleyg)?;
    let path = baleyg.join("workspace-id");
    match read_marker_during_creation(&path, hook) {
        Ok((id, file)) => {
            durable_marker(&path, &baleyg, git, &file, hook)?;
            Ok(id)
        }
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            let id = Uuid::new_v4();
            let created = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path);
            match created {
                Ok(mut f) => {
                    private_file(&path, &f)?;
                    hook(MarkerStage::CreatedBeforeWrite)?;
                    f.write_all(id.to_string().as_bytes())?;
                    durable_marker(&path, &baleyg, git, &f, hook)?;
                    Ok(id)
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let (id, file) = read_marker_during_creation(&path, hook)?;
                    durable_marker(&path, &baleyg, git, &file, hook)?;
                    Ok(id)
                }
                Err(e) => Err(e.into()),
            }
        }
        Err(e) => Err(e),
    }
}
#[derive(Debug)]
pub struct UseGuard {
    file: File,
    path: PathBuf,
    exclusive: bool,
}
impl UseGuard {
    pub fn acquire(path: &Path, exclusive: bool, nonblocking: bool) -> Result<Self> {
        Self::acquire_with_hook(path, exclusive, nonblocking, || Ok(()))
    }
    /// Fixture barrier after the first inode is opened, before flock.
    pub fn acquire_with_hook(
        path: &Path,
        exclusive: bool,
        nonblocking: bool,
        after_open: impl FnOnce() -> Result<()>,
    ) -> Result<Self> {
        Self::acquire_mode(path, exclusive, nonblocking, true, after_open)
    }
    pub fn acquire_existing_with_hook(
        path: &Path,
        exclusive: bool,
        nonblocking: bool,
        after_open: impl FnOnce() -> Result<()>,
    ) -> Result<Self> {
        Self::acquire_mode(path, exclusive, nonblocking, false, after_open)
    }
    pub fn acquire_existing(path: &Path, exclusive: bool, nonblocking: bool) -> Result<Self> {
        Self::acquire_existing_with_hook(path, exclusive, nonblocking, || Ok(()))
    }
    fn acquire_mode(
        path: &Path,
        exclusive: bool,
        nonblocking: bool,
        create: bool,
        after_open: impl FnOnce() -> Result<()>,
    ) -> Result<Self> {
        let flags = (if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        }) | (if nonblocking { libc::LOCK_NB } else { 0 });
        let mut hook = Some(after_open);
        for _ in 0..20 {
            let file = open_file(path, create)?;
            if let Some(after_open) = hook.take() {
                after_open()?;
            }
            let status = unsafe { libc::flock(file.as_raw_fd(), flags) };
            if status != 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::WouldBlock {
                    bail!("storage_busy: {}", path.display());
                }
                return Err(e.into());
            }
            let named = fs::symlink_metadata(path);
            if let Ok(m) = &named {
                let fd = file.metadata()?;
                if (m.dev(), m.ino()) == (fd.dev(), fd.ino()) {
                    private_file(path, &file)?;
                    return Ok(Self {
                        file,
                        path: path.to_owned(),
                        exclusive,
                    });
                }
            } else if !named
                .as_ref()
                .err()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                named?;
            }
        }
        bail!("lock pathname changed repeatedly: {}", path.display())
    }
    pub fn verify(&self) -> Result<()> {
        private_file(&self.path, &self.file)
    }
    pub fn remove_last(self) -> Result<()> {
        ensure!(self.exclusive, "exclusive use lock required for removal");
        self.verify()?;
        fs::remove_file(&self.path)?;
        sync_directory(self.path.parent().context("lock parent missing")?)
    }
}
impl Drop for UseGuard {
    fn drop(&mut self) {
        unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[derive(Debug)]
pub struct LeaderGuard {
    use_guard: UseGuard,
    file: File,
    path: PathBuf,
    pub incarnation: Uuid,
}
impl LeaderGuard {
    pub fn belongs_to(&self, leader_path: &Path) -> Result<()> {
        ensure!(self.path == leader_path, "storage_busy: wrong leader guard");
        self.verify()
    }
    pub fn verify(&self) -> Result<()> {
        self.use_guard.verify()?;
        private_file(&self.path, &self.file)
    }
}

impl Drop for LeaderGuard {
    fn drop(&mut self) {
        unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Lazy, versioned durable payloads. The legacy `Store::open` is replaced by the
/// topology cutover; this engine is independently usable until that cutover.
pub struct DurableRecords<'a> {
    roots: &'a TopologyRoots,
    identity: &'a WorkspaceIdentity,
}

const RECORD_SCHEMA: &str = "
CREATE TABLE record_metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1), schema_version INTEGER NOT NULL CHECK(schema_version=1), record_id TEXT NOT NULL, initialized INTEGER NOT NULL CHECK(initialized=1));
CREATE TABLE known_roots(path TEXT PRIMARY KEY, device TEXT NOT NULL, inode TEXT NOT NULL);
CREATE TABLE views(id TEXT PRIMARY KEY,payload TEXT NOT NULL);
CREATE TABLE annotations(id TEXT PRIMARY KEY,node_id TEXT NOT NULL,payload TEXT NOT NULL);
";

impl<'a> DurableRecords<'a> {
    pub fn new(roots: &'a TopologyRoots, identity: &'a WorkspaceIdentity) -> Self {
        Self { roots, identity }
    }
    fn existing(&self) -> Result<bool> {
        self.identity.verify()?;
        self.roots.reject_root_overlap(self.identity)?;
        for parent in [&self.roots.data, &self.roots.data.join("workspaces")] {
            match fs::symlink_metadata(parent) {
                Ok(_) => private_dir(parent)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(e) => return Err(e.into()),
            }
        }
        match fs::symlink_metadata(self.roots.record_dir(self.identity)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
            Ok(_) => {
                private_dir(&self.roots.data)?;
                private_dir(&self.roots.data.join("workspaces"))?;
                private_dir(&self.roots.record_dir(self.identity))?;
                Ok(true)
            }
        }
    }
    fn lock_existing(&self) -> Result<UseGuard> {
        UseGuard::acquire_existing(&self.roots.record_use_lock(self.identity), false, false)
            .context("incomplete_record: missing or unsafe use lock")
    }
    fn db(&self, writable: bool) -> Result<rusqlite::Connection> {
        use rusqlite::{Connection, OpenFlags};
        let path = self.roots.record_db(self.identity);
        ensure!(path.exists(), "incomplete_record: missing database");
        let file = open_file(&path, false)?;
        private_file(&path, &file)?;
        for suffix in ["-wal", "-shm", "-journal"] {
            let sidecar = path.with_file_name(format!("workspace.db{suffix}"));
            match fs::symlink_metadata(&sidecar) {
                Ok(_) => bail!("incomplete_record: recovery required"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        let flags = if writable {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let db = Connection::open_with_flags(&path, flags)?;
        private_file(&path, &file).context("incomplete_record: database changed")?;
        db.busy_timeout(Duration::ZERO)?;
        db.pragma_update(None, "temp_store", "MEMORY")?;
        if writable {
            db.pragma_update(None, "synchronous", "FULL")?;
        } else {
            db.pragma_update(None, "query_only", "ON")?;
        }
        let journal: String = db.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
        ensure!(journal == "delete", "incompatible_record: journal mode");
        let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        ensure!(version != 0, "incomplete_record: schema not committed");
        ensure!(version == 1, "incompatible_record: schema version");
        let row: (i64, String, i64) = db.query_row("SELECT schema_version,record_id,initialized FROM record_metadata WHERE singleton=1", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).context("incomplete_record: metadata")?;
        ensure!(
            row == (1, self.identity.record_id.clone(), 1),
            "incomplete_record: metadata mismatch"
        );
        db.prepare("SELECT path,device,inode FROM known_roots")?;
        db.prepare("SELECT id,payload FROM views")?;
        db.prepare("SELECT id,node_id,payload FROM annotations")?;
        self.identity.verify()?;
        Ok(db)
    }
    fn save(&self, table: &str, id: &str, node: Option<&str>, payload: String) -> Result<()> {
        self.save_with_first_save_hook(table, id, node, payload, |_| Ok(()))
    }
    fn save_with_first_save_hook(
        &self,
        table: &str,
        id: &str,
        node: Option<&str>,
        payload: String,
        mut hook: impl FnMut(&str) -> Result<()>,
    ) -> Result<()> {
        use rusqlite::{Connection, TransactionBehavior};
        let exists = self.existing()?;
        if !exists {
            self.roots.prepare_records(self.identity)?;
            let guard = self.roots.record_use(self.identity, true)?;
            self.identity.verify()?;
            // Another process may have completed creation before the lock was acquired.
            if self.roots.record_dir(self.identity).exists() {
                drop(guard);
                return self.save(table, id, node, payload);
            }
            make_private(&self.roots.record_dir(self.identity))?;
            let path = self.roots.record_db(self.identity);
            let _file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)?;
            let mut db = Connection::open(&path)?;
            db.busy_timeout(Duration::ZERO)?;
            db.pragma_update(None, "journal_mode", "DELETE")?;
            db.pragma_update(None, "synchronous", "FULL")?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(RECORD_SCHEMA)?;
            tx.pragma_update(None, "user_version", 1)?;
            tx.execute(
                "INSERT INTO record_metadata VALUES(1,1,?1,1)",
                [&self.identity.record_id],
            )?;
            Self::write_item(&tx, table, id, node, &payload)?;
            self.write_root(&tx)?;
            self.identity.verify()?;
            guard.verify()?;
            hook("before_commit")?;
            tx.commit()?;
            drop(db);
            hook("before_sync")?;
            open_file(&path, false)?.sync_all()?;
            sync_directory(&self.roots.record_dir(self.identity))?;
            sync_directory(&self.roots.data.join("workspaces"))?;
            sync_directory(&self.roots.data)?;
            return Ok(());
        }
        let guard = self.lock_existing()?;
        let mut db = self.db(true)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::write_item(&tx, table, id, node, &payload)?;
        self.write_root(&tx)?;
        self.identity.verify()?;
        guard.verify()?;
        tx.commit()?;
        Ok(())
    }
    fn write_item(
        db: &rusqlite::Transaction<'_>,
        table: &str,
        id: &str,
        node: Option<&str>,
        payload: &str,
    ) -> Result<()> {
        use rusqlite::params;
        if table == "views" {
            db.execute("INSERT INTO views VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload", params![id,payload])?;
        } else {
            db.execute("INSERT INTO annotations VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET node_id=excluded.node_id,payload=excluded.payload", params![id,node,payload])?;
        }
        Ok(())
    }
    fn write_root(&self, db: &rusqlite::Transaction<'_>) -> Result<()> {
        db.execute("INSERT INTO known_roots VALUES(?1,?2,?3) ON CONFLICT(path) DO UPDATE SET device=excluded.device,inode=excluded.inode", rusqlite::params![self.identity.root.to_str().context("non-UTF8 root")?,self.identity.device.to_string(),self.identity.inode.to_string()])?;
        Ok(())
    }
    fn list<T: serde::de::DeserializeOwned>(&self, sql: &str) -> Result<Vec<T>> {
        if !self.existing()? {
            return Ok(vec![]);
        }
        let guard = self.lock_existing()?;
        let mut db = self.db(false)?;
        let tx = db.transaction()?;
        let values = tx
            .prepare(sql)?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let result = values
            .iter()
            .map(|value| serde_json::from_str(value).map_err(Into::into))
            .collect();
        self.identity.verify()?;
        guard.verify()?;
        result
    }
    pub fn views(&self) -> Result<Vec<crate::model::SavedView>> {
        self.list("SELECT payload FROM views ORDER BY id")
    }
    pub fn annotations(&self) -> Result<Vec<crate::model::Annotation>> {
        self.list("SELECT payload FROM annotations ORDER BY id")
    }
    pub fn view(&self, id: &str) -> Result<Option<crate::model::SavedView>> {
        Ok(self.views()?.into_iter().find(|v| v.id == id))
    }
    pub fn annotation(&self, id: &str) -> Result<Option<crate::model::Annotation>> {
        Ok(self.annotations()?.into_iter().find(|a| a.id == id))
    }
    pub fn put_view(&self, view: &crate::model::SavedView) -> Result<()> {
        view.validate()?;
        self.save("views", &view.id, None, serde_json::to_string(view)?)
    }
    pub fn put_annotation(&self, annotation: &crate::model::Annotation) -> Result<()> {
        self.put_annotation_with_first_save_hook(annotation, |_| Ok(()))
    }
    /// Fixture fault at the first record's commit or post-commit sync boundary.
    pub fn put_annotation_with_first_save_hook(
        &self,
        annotation: &crate::model::Annotation,
        hook: impl FnMut(&str) -> Result<()>,
    ) -> Result<()> {
        annotation.validate()?;
        self.save_with_first_save_hook(
            "annotations",
            &annotation.id,
            Some(&annotation.node_id),
            serde_json::to_string(annotation)?,
            hook,
        )
    }
    fn delete(&self, table: &str, id: &str) -> Result<bool> {
        if !self.existing()? {
            return Ok(false);
        }
        let guard = self.lock_existing()?;
        let mut db = self.db(true)?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = if table == "views" {
            tx.execute("DELETE FROM views WHERE id=?1", [id])?
        } else {
            tx.execute("DELETE FROM annotations WHERE id=?1", [id])?
        };
        self.identity.verify()?;
        guard.verify()?;
        tx.commit()?;
        Ok(changed != 0)
    }
    pub fn delete_view(&self, id: &str) -> Result<bool> {
        self.delete("views", id)
    }
    pub fn delete_annotation(&self, id: &str) -> Result<bool> {
        self.delete("annotations", id)
    }
}

#[cfg(test)]
pub fn assert_topology_fixture(store: &crate::store::Store, state: &Path) {
    let status = store.status().unwrap();
    let identity = WorkspaceIdentity::discover(
        Some(Path::new(&status.workspace_root)),
        Path::new(&status.workspace_root),
    )
    .unwrap();
    let roots = TopologyRoots::isolated_for_tests(state.join("cache"), state.join("data"));
    let index = roots.index_dir(&identity);
    assert_eq!(index.parent().unwrap(), state.join("cache/indexes"));
    for path in [&roots.cache, &roots.cache.join("indexes"), &index] {
        private_dir(path).unwrap();
    }
    for path in [
        roots.index_db(&identity),
        roots.index_use_lock(&identity),
        roots.leader_lock(&identity),
    ] {
        let file = open_file(&path, false).unwrap();
        private_file(&path, &file).unwrap();
    }
    assert!(
        !roots.record_db(&identity).exists(),
        "fixture open must not eagerly create a durable record"
    );
    let shared =
        UseGuard::acquire_existing(&roots.index_use_lock(&identity), false, false).unwrap();
    assert!(UseGuard::acquire_existing(&roots.index_use_lock(&identity), true, true).is_err());
    drop(shared);
}
