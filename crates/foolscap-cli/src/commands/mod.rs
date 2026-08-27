//! One module per subcommand. Each exposes an `Args` struct and a `run`.

pub mod compress;
pub mod info;
pub mod merge;
pub mod meta;
pub mod optimize;
#[cfg(feature = "render")]
pub mod render;
pub mod rotate;
pub mod split;
