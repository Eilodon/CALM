//! Filesystem primitives that need a stronger containment guarantee than
//! `path_policy`'s textual canonicalize-then-compare check can give -- see
//! `rooted`'s own doc comment for what that gap is and how this closes it.

pub mod rooted;

pub use rooted::{ContainmentMethod, RootedFilesystem, RootedFsError, WriteReceipt};
