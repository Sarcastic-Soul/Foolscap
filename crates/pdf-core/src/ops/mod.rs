//! Document operations.
//!
//! Each operation is a free function taking already-opened inputs and returning
//! a typed result. None of them print, prompt, or exit.

pub mod arrange;
pub mod compress;
pub(crate) mod contents;
pub mod crop;
pub mod encrypt;
pub mod extract_images;
pub mod filter;
pub mod flatten;
pub(crate) mod image;
pub mod linearize;
pub mod merge;
pub mod metadata;
pub mod nup;
pub mod optimize;
#[cfg(feature = "render")]
pub mod overlay;
pub mod redact;
pub mod repair;
pub mod rotate;
pub mod split;
pub mod stamp;
pub mod strip;
#[cfg(feature = "render")]
pub mod svg;

pub use arrange::{arrange, delete, move_pages};
pub use compress::{compress, compress_with_progress, CompressLevel, CompressReport, SkipReason};
pub use crop::{
    crop, crop_margins, crop_margins_with_progress, crop_with_progress, reset_crop, scale_to,
    scale_to_with_progress, set_mediabox, set_mediabox_with_progress, Fit, Margins, PageSize, Rect,
};
pub use encrypt::{encrypt, EncryptOptions, Permissions};
pub use extract_images::extract_images;
pub use filter::{filter, filter_with_progress, Filter, FilterReport};
pub use flatten::{flatten, flatten_with_progress, FlattenOptions, FlattenReport};
pub use linearize::{is_linearized, linearize, linearize_with_progress};
pub use merge::{merge, merge_with_progress};
pub use metadata::MetadataEdit;
pub use nup::{booklet, booklet_with_progress, nup, nup_with_progress, Binding, BookletOptions, NupOptions, PageOrder};
pub use optimize::{optimize, OptimizeLevel, OptimizeReport};
#[cfg(feature = "render")]
pub use overlay::overlay;
pub use redact::{
    redact, redact_find, redact_find_with_progress, redact_with_progress, RedactReport,
};
pub use repair::{repair, repair_loaded, repair_with, RepairReport};
pub use rotate::rotate;
pub use split::{
    plan as split_plan, plan_with_options as split_plan_with_options, split, split_with_options,
    split_with_progress, SplitOptions, SplitSpec,
};
pub use stamp::{
    anchor_point, estimate_text_width, expand_template, rotation_matrix, stamp,
    stamp_with_progress, Anchor, Point, StampContent, StampFont, StampLayer, StampOptions, Stroke,
};
pub use strip::{strip, StripReport};
#[cfg(feature = "render")]
pub use svg::{plan, to_svg, to_svg_with_progress, FontEmbedding, SvgOptions};