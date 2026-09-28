//! Dependency declarations are not evidence of a call target or type edge.
use crate::{behavior::SequenceView, dependencies::Catalog, model::SourceFile};

/// Keep measured terminal sequence steps unchanged. Dependency name matches
/// cannot add participants, targets, or dispatch information.
pub fn annotate(_view: &mut SequenceView, _file: &SourceFile, _catalog: &Catalog) {}
