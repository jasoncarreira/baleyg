//! Shared admission and publication boundary for CLI and authenticated daemon index jobs.
//! A job has one expected pair, one captured source set, and one complete paired commit.
use crate::{
    indexer::{self, IndexOptions},
    model::{CancelFlag, IndexPin, IndexProgress},
    store::{Store, topology::LeaderGuard},
};
use anyhow::{Result, ensure};
use std::sync::atomic::Ordering;

pub struct IndexJobCoordinator {
    store: Store,
    expected: IndexPin,
    leader: LeaderGuard,
}

impl IndexJobCoordinator {
    /// The control baseline admits known old indexes without exposing their evidence to readers.
    /// A supplied HTTP pair is checked before any source admission or worker is started.
    pub fn prepare(store: &Store, requested: Option<IndexPin>) -> Result<Self> {
        let expected = store.index_baseline()?;
        ensure!(
            requested.is_none_or(|pin| pin == expected),
            "revision conflict"
        );
        let leader = store.leader()?;
        Ok(Self {
            store: store.clone(),
            expected,
            leader,
        })
    }

    /// Projection uses the admitted bytes; publication checks drift, cancellation and the
    /// whole expected pair under the writer lock before making graph and native rows visible.
    pub fn run(
        self,
        options: &IndexOptions,
        cancel: &CancelFlag,
        progress: impl Fn(IndexProgress) + Sync,
    ) -> Result<IndexPin> {
        ensure!(!cancel.load(Ordering::Acquire), "index cancelled");
        let (graph, native, capture) =
            indexer::index_workspace_bundle(options, self.store.root_id(), cancel, progress)?;
        ensure!(!cancel.load(Ordering::Acquire), "index cancelled");
        self.store.publish_native(
            &graph,
            &capture,
            &native,
            &self.leader,
            self.expected,
            cancel,
        )
    }
}
