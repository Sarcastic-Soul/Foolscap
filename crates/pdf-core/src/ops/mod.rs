//! Document operations.
//!
//! Each operation is a free function taking already-opened inputs and returning
//! a typed result. None of them print, prompt, or exit.

pub mod merge;
pub mod metadata;
pub mod optimize;
pub mod rotate;
pub mod split;

pub use merge::merge;
pub use optimize::{optimize, OptimizeLevel, OptimizeReport};
pub use rotate::rotate;
pub use split::{split, SplitSpec};
