//! In-memory checkout admission. Retained entries never own checkout resources.
use crate::store::topology::WorkspaceIdentity;
use std::{
    collections::{HashMap, HashSet},
    fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

pub const MAX_ACTIVE_CHECKOUTS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionError {
    NotAbsolute,
    NotCheckout,
    DifferentRepository,
    IdentityChanged,
    Unavailable,
    CheckoutCapacity,
    RegistrationConflict,
}
impl SelectionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::CheckoutCapacity => "checkout_capacity",
            Self::RegistrationConflict => "registration_conflict",
            _ => "workspace_selection_failed",
        }
    }
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NotAbsolute => "not_absolute",
            Self::NotCheckout => "not_checkout",
            Self::DifferentRepository => "different_repository",
            Self::IdentityChanged => "identity_changed",
            Self::Unavailable => "unavailable",
            Self::CheckoutCapacity => "checkout_capacity",
            Self::RegistrationConflict => "registration_conflict",
        }
    }
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Unavailable | Self::CheckoutCapacity)
    }
}

/// Options are inert data, not a Store, index handle, watcher or leader lease.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckoutOptions(pub serde_json::Value);

struct Entry {
    identity: Arc<WorkspaceIdentity>,
    registration: Option<CheckoutOptions>,
    sessions: HashSet<u64>,
}

#[derive(Default)]
pub struct CheckoutRegistry {
    entries: HashMap<String, Entry>,
}

impl CheckoutRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn known_roots(&self) -> Vec<(String, PathBuf)> {
        let mut roots: Vec<_> = self
            .entries
            .iter()
            .map(|(key, entry)| (key.clone(), entry.identity.root.clone()))
            .collect();
        roots.sort();
        roots
    }

    pub fn active_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| !entry.sessions.is_empty())
            .count()
    }

    pub fn registration(&self, key: &str) -> Option<&CheckoutOptions> {
        self.entries.get(key)?.registration.as_ref()
    }

    /// A repeat is idempotent. Replacement is possible only after release.
    pub fn register(
        &mut self,
        identity: &WorkspaceIdentity,
        options: CheckoutOptions,
    ) -> Result<(), SelectionError> {
        identity
            .verify_readonly()
            .map_err(|_| SelectionError::IdentityChanged)?;
        let key = &identity.root_key;
        if let Some(entry) = self.entries.get_mut(key) {
            entry
                .identity
                .verify_readonly()
                .map_err(|_| SelectionError::IdentityChanged)?;
            if entry.registration.as_ref() == Some(&options) {
                return Ok(());
            }
            if !entry.sessions.is_empty() {
                return Err(SelectionError::RegistrationConflict);
            }
            entry.registration = Some(options);
            return Ok(());
        }
        self.entries.insert(
            key.clone(),
            Entry {
                identity: Arc::new(
                    identity
                        .verified_clone()
                        .map_err(|_| SelectionError::IdentityChanged)?,
                ),
                registration: Some(options),
                sessions: HashSet::new(),
            },
        );
        Ok(())
    }

    /// The caller retains its launch identity for the lifetime of its connection.
    /// This operation does not open a Store or elect a checkout leader.
    pub fn attach_launch(
        &mut self,
        session: u64,
        launch: &WorkspaceIdentity,
    ) -> Result<Arc<WorkspaceIdentity>, SelectionError> {
        launch
            .verify_readonly()
            .map_err(|_| SelectionError::IdentityChanged)?;
        self.attach(session, launch)
    }

    pub fn select(
        &mut self,
        session: u64,
        launch: &WorkspaceIdentity,
        selected: &Path,
    ) -> Result<Arc<WorkspaceIdentity>, SelectionError> {
        launch
            .verify_readonly()
            .map_err(|_| SelectionError::IdentityChanged)?;
        let root = checkout_root(selected)?;
        let identity = WorkspaceIdentity::discover(Some(&root), &root)
            .and_then(WorkspaceIdentity::attach_marker)
            .map_err(|_| SelectionError::Unavailable)?;
        let launch_common = common_identity(launch)?;
        let selected_common = common_identity(&identity)?;
        if launch_common != selected_common {
            return Err(SelectionError::DifferentRepository);
        }
        // The worktree and its common directory can change while Git runs.
        launch
            .verify_readonly()
            .map_err(|_| SelectionError::IdentityChanged)?;
        identity
            .verify_readonly()
            .map_err(|_| SelectionError::IdentityChanged)?;
        if common_identity(launch)? != launch_common
            || common_identity(&identity)? != selected_common
        {
            return Err(SelectionError::IdentityChanged);
        }
        self.attach(session, &identity)
    }

    fn attach(
        &mut self,
        session: u64,
        identity: &WorkspaceIdentity,
    ) -> Result<Arc<WorkspaceIdentity>, SelectionError> {
        let at_capacity = self.active_count() >= MAX_ACTIVE_CHECKOUTS;
        if let Some(entry) = self.entries.get_mut(&identity.root_key) {
            entry
                .identity
                .verify_readonly()
                .map_err(|_| SelectionError::IdentityChanged)?;
            if (entry.identity.device, entry.identity.inode) != (identity.device, identity.inode)
                || entry.identity.record_id != identity.record_id
            {
                return Err(SelectionError::IdentityChanged);
            }
            if entry.sessions.is_empty() && at_capacity {
                return Err(SelectionError::CheckoutCapacity);
            }
            entry.sessions.insert(session);
            return Ok(Arc::clone(&entry.identity));
        }
        if at_capacity {
            return Err(SelectionError::CheckoutCapacity);
        }
        let identity = Arc::new(
            identity
                .verified_clone()
                .map_err(|_| SelectionError::IdentityChanged)?,
        );
        self.entries.insert(
            identity.root_key.clone(),
            Entry {
                identity: Arc::clone(&identity),
                registration: None,
                sessions: HashSet::from([session]),
            },
        );
        Ok(identity)
    }

    /// Called at MCP EOF/disconnect; an explicit worktree remains attached until then.
    pub fn disconnect(&mut self, session: u64) {
        for entry in self.entries.values_mut() {
            entry.sessions.remove(&session);
        }
    }
}

fn common_identity(identity: &WorkspaceIdentity) -> Result<(u64, u64), SelectionError> {
    let path = identity
        .git_common_dir()
        .map_err(|_| SelectionError::Unavailable)?;
    let metadata = fs::metadata(path).map_err(|_| SelectionError::Unavailable)?;
    Ok((metadata.dev(), metadata.ino()))
}

/// Walk the literal selected pathname. In particular, do not canonicalize a
/// symlink (including one in an intermediate component) into a different root.
fn checkout_root(selected: &Path) -> Result<PathBuf, SelectionError> {
    if !selected.is_absolute() {
        return Err(SelectionError::NotAbsolute);
    }
    let mut literal = PathBuf::new();
    for component in selected.components() {
        match component {
            Component::RootDir | Component::Normal(_) => literal.push(component),
            _ => return Err(SelectionError::NotCheckout),
        }
        let metadata = fs::symlink_metadata(&literal).map_err(|_| SelectionError::Unavailable)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(SelectionError::NotCheckout);
        }
    }
    for candidate in literal.ancestors() {
        match fs::symlink_metadata(candidate.join(".git")) {
            Ok(metadata) if metadata.is_dir() || metadata.is_file() => {
                if metadata.file_type().is_symlink() {
                    return Err(SelectionError::NotCheckout);
                }
                return Ok(candidate.to_path_buf());
            }
            Ok(_) => return Err(SelectionError::NotCheckout),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(SelectionError::Unavailable),
        }
    }
    Err(SelectionError::NotCheckout)
}
