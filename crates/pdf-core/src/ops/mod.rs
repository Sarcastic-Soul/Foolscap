//! Document operations.
//!
//! Each operation is a free function taking already-opened inputs and returning
//! a typed result. None of them print, prompt, or exit.

pub mod merge;
pub mod metadata;
pub mod optimize;
pub mod rotate;
pub mod split;

pub use merge::{merge, merge_with_progress};
pub use metadata::MetadataEdit;
pub use optimize::{optimize, OptimizeLevel, OptimizeReport};
pub use rotate::rotate;
pub use split::{plan as split_plan, split, split_with_progress, SplitSpec};
