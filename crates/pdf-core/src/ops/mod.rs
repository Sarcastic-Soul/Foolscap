//! Document operations.
//!
//! Each operation is a free function taking already-opened inputs and returning
//! a typed result. None of them print, prompt, or exit.

pub mod arrange;
pub mod compress;
pub(crate) mod contents;
pub mod crop;
pub mod encrypt;
pub mod filter;
pub mod flatten;
pub(crate) mod image;
pub mod merge;
pub mod metadata;
pub mod optimize;
pub mod redact;
pub mod repair;
pub mod rotate;
pub mod split;
pub mod stamp;
pub mod strip;

pub use arrange::{arrange, delete, move_pages};
pub use compress::{compress, compress_with_progress, CompressLevel, CompressReport, SkipReason};
pub use crop::{
    crop, crop_margins, crop_margins_with_progress, crop_with_progress, reset_crop, scale_to,
    scale_to_with_progress, set_mediabox, set_mediabox_with_progress, Fit, Margins, PageSize, Rect,
};
pub use encrypt::{encrypt, EncryptOptions, Permissions};
pub use filter::{filter, filter_with_progress, Filter, FilterReport};
pub use flatten::{flatten, flatten_with_progress, FlattenOptions, FlattenReport};
pub use merge::{merge, merge_with_progress};
pub use metadata::MetadataEdit;
pub use optimize::{optimize, OptimizeLevel, OptimizeReport};
pub use redact::{
    redact, redact_find, redact_find_with_progress, redact_with_progress, RedactReport,
};
pub use repair::{repair, repair_loaded, repair_with, RepairReport};
pub use rotate::rotate;
pub use split::{plan as split_plan, split, split_with_progress, SplitSpec};
pub use stamp::{
    anchor_point, estimate_text_width, expand_template, rotation_matrix, stamp,
    stamp_with_progress, Anchor, Point, StampContent, StampFont, StampLayer, StampOptions, Stroke,
};
pub use strip::{strip, StripReport};
