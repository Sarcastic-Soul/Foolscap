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
}
