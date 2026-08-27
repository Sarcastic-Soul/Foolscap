//! One module per subcommand. Each exposes an `Args` struct and a `run`.

pub mod compress;
#[cfg(feature = "convert")]
pub mod convert;
pub mod info;
pub mod merge;
pub mod meta;
#[cfg(feature = "ocr")]
pub mod ocr;
pub mod optimize;
#[cfg(feature = "render")]
pub mod render;
pub mod rotate;
pub mod split;
#[cfg(all(feature = "convert", feature = "render"))]
pub mod to_images;
