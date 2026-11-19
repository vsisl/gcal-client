//! Profile pictures: turning a downloaded picture into a round one the window can show,
//! and the coloured letter shown for people without a picture.
//!
//! Pictures are kept in memory only, for as long as the app runs.

use std::io::Cursor;

use image::{ImageReader, Limits, imageops::FilterType};
use slint::{Color, Rgba8Pixel, SharedPixelBuffer};

/// The size pictures are asked for and kept at, in pixels: twice the largest circle the
/// window draws, so that they stay sharp on a high-density screen.
pub const PICTURE_SIZE: u32 = 96;

/// The largest picture, in pixels a side, that is decoded at all. Pictures come from
/// other people's profiles; one claiming to be huge is not worth the memory.
const MAX_SIDE: u32 = 2048;

/// Decodes a downloaded picture into a round one of `PICTURE_SIZE` pixels a side: the
/// middle square of it, with what lies outside the circle transparent. `None` if it is
/// not a picture the app can read.
pub fn round_picture(bytes: &[u8]) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    reader.limits(limits);
    let picture = reader.decode().ok()?;
    let side = picture.width().min(picture.height());
    if side == 0 {
        return None;
    }
    let square = picture.crop_imm(
        (picture.width() - side) / 2,
        (picture.height() - side) / 2,
        side,
        side,
    );
    let mut pixels = square
        .resize_exact(PICTURE_SIZE, PICTURE_SIZE, FilterType::Triangle)
        .into_rgba8();

    // The circle is cut here rather than in the window, so that it looks the same
    // whichever way the window is drawn. The edge pixel is blended for a smooth rim.
    let radius = PICTURE_SIZE as f32 / 2.0;
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        let dx = x as f32 + 0.5 - radius;
        let dy = y as f32 + 0.5 - radius;
        let inside = (radius - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
        pixel[3] = (pixel[3] as f32 * inside).round() as u8;
    }
    Some(SharedPixelBuffer::clone_from_slice(
        pixels.as_raw(),
        PICTURE_SIZE,
        PICTURE_SIZE,
    ))
}

/// The letter shown for someone without a picture: the first letter of their name, or
/// of their address if no name is known.
pub fn initial(name: &str, email: &str) -> String {
    let source = if name.trim().is_empty() { email } else { name };
    source
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default()
}

/// Colours for the letters, in the spirit of Google's: each person gets one of them,
/// always the same for the same address.
const TINTS: [u32; 10] = [
    0x1a73e8, 0xd93025, 0x188038, 0xe37400, 0x9334e6, 0x007b83, 0xc5221f, 0x3c4043, 0xb06000,
    0x1967d2,
];

/// The colour of someone's letter, chosen by their address.
pub fn tint(email: &str) -> Color {
    // A small, fixed hash: the same address must get the same colour on every start.
    let hash = email.to_lowercase().bytes().fold(0u32, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as u32)
    });
    let rgb = TINTS[hash as usize % TINTS.len()];
    Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, RgbaImage};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let picture = RgbaImage::from_pixel(width, height, image::Rgba([200, 10, 10, 255]));
        let mut bytes = Vec::new();
        picture
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn a_picture_becomes_a_round_one_of_the_kept_size() {
        let round = round_picture(&png(120, 80)).unwrap();
        assert_eq!(
            (round.width(), round.height()),
            (PICTURE_SIZE, PICTURE_SIZE)
        );
        let at = |x: u32, y: u32| round.as_slice()[(y * PICTURE_SIZE + x) as usize];
        // The middle keeps the picture; the corners are cut away.
        let middle = at(PICTURE_SIZE / 2, PICTURE_SIZE / 2);
        assert_eq!((middle.r, middle.a), (200, 255));
        assert_eq!(at(0, 0).a, 0);
        assert_eq!(at(PICTURE_SIZE - 1, PICTURE_SIZE - 1).a, 0);
    }

    #[test]
    fn what_is_not_a_readable_picture_gives_none() {
        assert!(round_picture(b"<html>not a picture</html>").is_none());
        assert!(round_picture(&[]).is_none());
        // One claiming to be larger than any profile picture is not decoded.
        assert!(round_picture(&png(MAX_SIDE + 1, 1)).is_none());
    }

    #[test]
    fn the_letter_is_of_the_name_or_else_of_the_address() {
        assert_eq!(initial("Ann Lee", "x@example.com"), "A");
        assert_eq!(initial("", "bob@example.com"), "B");
        assert_eq!(initial("  ", "\"quoted\"@example.com"), "Q");
        // Not cut in the middle of a character, and in capitals where there are any.
        assert_eq!(initial("šárka", ""), "Š");
        assert_eq!(initial("李雷", ""), "李");
        assert_eq!(initial("", ""), "");
    }

    #[test]
    fn the_same_address_always_gets_the_same_colour() {
        assert_eq!(tint("Ann@Example.com"), tint("ann@example.com"));
        let colours: std::collections::HashSet<_> = ["a@x.org", "b@x.org", "c@x.org", "d@x.org"]
            .iter()
            .map(|email| {
                let colour = tint(email);
                (colour.red(), colour.green(), colour.blue())
            })
            .collect();
        assert!(colours.len() > 1);
    }
}
