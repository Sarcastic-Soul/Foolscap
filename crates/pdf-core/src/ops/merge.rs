use std::path::{Path, PathBuf};

use crate::error::Result;

/// Concatenate `inputs` in order into a single document at `output`.
///
/// The result has exactly the sum of the inputs' page counts.
pub fn merge(_inputs: &[PathBuf], _output: &Path) -> Result<()> {
    todo!("stage 1: merge")
}
