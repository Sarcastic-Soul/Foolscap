//! Turning rendered pages into something GTK can draw.

use gtk4::gdk;
use gtk4::glib;
use pdf_core::render::RenderedPage;

/// Wrap a rendered page as a texture, rotating it by a quarter turn as needed.
///
/// Rotation is applied to the pixels here rather than by re-rendering, so that
/// turning a page is instant and costs no work in MuPDF. The real rotation is
/// written into the document only when the user saves.
pub fn texture(page: &RenderedPage, rotation: i32) -> gdk::MemoryTexture {
    let (width, height, pixels) = rotate(page, rotation.rem_euclid(360));

    gdk::MemoryTexture::new(
        width as i32,
        height as i32,
        memory_format(page.channels),
        &glib::Bytes::from_owned(pixels),
        width as usize * page.channels as usize,
    )
}

fn memory_format(channels: u8) -> gdk::MemoryFormat {
    match channels {
        3 => gdk::MemoryFormat::R8g8b8,
        // The renderer always produces RGBA; anything else is a surprise, and
        // guessing wrong shows as swapped colour channels rather than a crash.
        _ => gdk::MemoryFormat::R8g8b8a8,
    }
}

/// Rotate a pixel buffer clockwise, returning the new geometry.
fn rotate(page: &RenderedPage, degrees: i32) -> (u32, u32, Vec<u8>) {
    let channels = page.channels as usize;
    let width = page.width as usize;
    let height = page.height as usize;

    if degrees == 0 || width == 0 || height == 0 {
        return (page.width, page.height, page.pixels.clone());
    }

    let (new_width, new_height) = match degrees {
        90 | 270 => (height, width),
        _ => (width, height),
    };

    let mut rotated = vec![0u8; new_width * new_height * channels];

    for y in 0..height {
        for x in 0..width {
            let (target_x, target_y) = match degrees {
                90 => (height - 1 - y, x),
                180 => (width - 1 - x, height - 1 - y),
                270 => (y, width - 1 - x),
                _ => (x, y),
            };

            let from = (y * width + x) * channels;
            let to = (target_y * new_width + target_x) * channels;
            rotated[to..to + channels].copy_from_slice(&page.pixels[from..from + channels]);
        }
    }

    (new_width as u32, new_height as u32, rotated)
}

/// Two pages laid over each other, with the differences marked.
pub struct Diff {
    /// The pages as they were put side by side, in RGBA.
    pub overlay: RenderedPage,
    /// How many pixels differ by more than the tolerance.
    pub changed: usize,
    /// Pixels compared in all.
    pub total: usize,
}

impl Diff {
    /// Share of the page that differs, as a percentage.
    pub fn percent(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            self.changed as f32 * 100.0 / self.total as f32
        }
    }
}

/// How far apart two colour values can be before they count as different.
/// A little slack hides the faint edge noise that rendering the same page
/// twice at slightly different sizes leaves behind.
const DIFF_TOLERANCE: i32 = 24;

/// Compare two rendered pages pixel by pixel.
///
/// The result is the left page, faded, with every pixel that differs from the
/// right page painted red. Returns `None` when the pages are not the same size,
/// because then there is no honest way to line the pixels up; the caller says
/// so rather than showing a misleading mark.
pub fn diff(left: &RenderedPage, right: &RenderedPage) -> Option<Diff> {
    if left.width != right.width
        || left.height != right.height
        || left.channels != right.channels
        || left.channels < 3
    {
        return None;
    }

    let channels = left.channels as usize;
    let count = left.width as usize * left.height as usize;
    let mut pixels = Vec::with_capacity(count * 4);
    let mut changed = 0;

    for at in 0..count {
        let a = &left.pixels[at * channels..at * channels + 3];
        let b = &right.pixels[at * channels..at * channels + 3];
        let differs = a
            .iter()
            .zip(b)
            .any(|(x, y)| (i32::from(*x) - i32::from(*y)).abs() > DIFF_TOLERANCE);

        if differs {
            changed += 1;
            pixels.extend_from_slice(&[220, 38, 38, 255]);
        } else {
            // Fade the shared content toward white so the marks stand out.
            for value in a {
                pixels.push(((u16::from(*value) + 255 * 2) / 3) as u8);
            }
            pixels.push(255);
        }
    }

    Some(Diff {
        overlay: RenderedPage {
            width: left.width,
            height: left.height,
            channels: 4,
            pixels,
        },
        changed,
        total: count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-by-one image, so that orientation is unambiguous.
    fn wide() -> RenderedPage {
        RenderedPage {
            width: 2,
            height: 1,
            channels: 1,
            pixels: vec![1, 2],
        }
    }

    #[test]
    fn no_rotation_leaves_the_buffer_alone() {
        let (width, height, pixels) = rotate(&wide(), 0);
        assert_eq!((width, height), (2, 1));
        assert_eq!(pixels, vec![1, 2]);
    }

    #[test]
    fn a_quarter_turn_swaps_the_axes() {
        let (width, height, pixels) = rotate(&wide(), 90);
        assert_eq!((width, height), (1, 2));
        // The left pixel ends up at the top.
        assert_eq!(pixels, vec![1, 2]);
    }

    #[test]
    fn a_half_turn_reverses_the_row() {
        let (width, height, pixels) = rotate(&wide(), 180);
        assert_eq!((width, height), (2, 1));
        assert_eq!(pixels, vec![2, 1]);
    }

    #[test]
    fn three_quarters_swaps_the_axes_the_other_way() {
        let (width, height, pixels) = rotate(&wide(), 270);
        assert_eq!((width, height), (1, 2));
        assert_eq!(pixels, vec![2, 1]);
    }

    #[test]
    fn four_quarter_turns_return_the_original() {
        let original = RenderedPage {
            width: 3,
            height: 2,
            channels: 1,
            pixels: vec![1, 2, 3, 4, 5, 6],
        };

        let mut current = original.clone();
        for _ in 0..4 {
            let (width, height, pixels) = rotate(&current, 90);
            current = RenderedPage {
                width,
                height,
                channels: 1,
                pixels,
            };
        }

        assert_eq!(current, original);
    }

    #[test]
    fn a_multi_channel_buffer_keeps_its_channels_together() {
        let page = RenderedPage {
            width: 2,
            height: 1,
            channels: 4,
            pixels: vec![1, 2, 3, 4, 5, 6, 7, 8],
        };

        let (_, _, pixels) = rotate(&page, 180);
        assert_eq!(pixels, vec![5, 6, 7, 8, 1, 2, 3, 4]);
    }

    #[test]
    fn an_empty_buffer_does_not_panic() {
        let page = RenderedPage {
            width: 0,
            height: 0,
            channels: 4,
            pixels: Vec::new(),
        };
        let (width, height, pixels) = rotate(&page, 90);
        assert_eq!((width, height), (0, 0));
        assert!(pixels.is_empty());
    }

    fn flat(width: u32, height: u32, value: u8) -> RenderedPage {
        RenderedPage {
            width,
            height,
            channels: 3,
            pixels: vec![value; (width * height * 3) as usize],
        }
    }

    #[test]
    fn identical_pages_have_no_differences() {
        let page = flat(4, 4, 200);
        let result = diff(&page, &page).unwrap();
        assert_eq!(result.changed, 0);
        assert_eq!(result.total, 16);
        assert_eq!(result.percent(), 0.0);
        assert_eq!(result.overlay.channels, 4);
    }

    #[test]
    fn a_changed_pixel_is_marked_red() {
        let left = flat(2, 1, 255);
        let mut right = flat(2, 1, 255);
        right.pixels[3..6].copy_from_slice(&[0, 0, 0]);

        let result = diff(&left, &right).unwrap();
        assert_eq!(result.changed, 1);
        assert_eq!(&result.overlay.pixels[4..8], &[220, 38, 38, 255]);
        assert_eq!(&result.overlay.pixels[0..4], &[255, 255, 255, 255]);
        assert_eq!(result.percent(), 50.0);
    }

    #[test]
    fn faint_noise_is_ignored() {
        let result = diff(&flat(3, 3, 200), &flat(3, 3, 210)).unwrap();
        assert_eq!(result.changed, 0);
    }

    #[test]
    fn pages_of_different_sizes_cannot_be_compared() {
        assert!(diff(&flat(2, 2, 0), &flat(3, 2, 0)).is_none());
    }

    #[test]
    fn a_greyscale_page_cannot_be_compared() {
        let grey = RenderedPage {
            width: 1,
            height: 1,
            channels: 1,
            pixels: vec![0],
        };
        assert!(diff(&grey, &grey).is_none());
    }
}
