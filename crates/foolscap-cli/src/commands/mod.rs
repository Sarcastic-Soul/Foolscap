//! One module per subcommand. Each exposes an `Args` struct and a `run`.

pub mod arrange;
pub mod booklet;
pub mod compress;
#[cfg(feature = "convert")]
pub mod convert;
pub mod crop;
pub mod decrypt;
pub mod encrypt;
pub mod extract_images;
pub mod filter;
pub mod flatten;
pub mod info;
pub mod merge;
pub mod meta;
pub mod nup;
#[cfg(feature = "ocr")]
pub mod ocr;
pub mod optimize;
#[cfg(feature = "render")]
pub mod overlay;
pub mod redact;
#[cfg(feature = "render")]
pub mod render;
pub mod repair;
pub mod rotate;
pub mod split;
pub mod stamp;
pub mod strip;
#[cfg(all(feature = "convert", feature = "render"))]
pub mod to_images;
