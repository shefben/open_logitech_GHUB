//! The notification-area icon, as raw pixels.
//!
//! The window decodes PNGs with the `image` crate, because it has hundreds of icons to draw. The
//! agent has exactly one, and pulling in a PNG decoder plus its `zlib` for a single 64x64 mask
//! would be the largest single dependency in a binary whose point is to be small. So the G mark
//! ships twice: as `assets/icons/g-logo.png` for the window, and as `assets/tray/g-logo.rgba` -
//! the same pixels, already decoded - for this one.
//!
//! Two copies of one asset can drift, so they are tied together by a test in `ghub-ui`, which has
//! the decoder: it decodes the PNG and asserts the result is byte-for-byte what this file reads.
//! A regenerated or re-extracted G mark therefore fails the suite rather than quietly leaving the
//! agent on the old artwork.

use ghub_platform::tray::TrayImage;

/// `width: u32` then `height: u32`, both little-endian, then `width * height * 4` bytes of RGBA
/// with the top row first - the layout `TrayImage` wants and `CreateDIBSection` consumes.
const HEADER: usize = 8;

/// Where the blob lives, relative to the assets directory.
pub const ASSET: &str = "tray/g-logo.rgba";

/// Loads the tray icon, or says why it could not be loaded.
pub fn tray_image() -> Result<TrayImage, String> {
    let path = asset_root().join(ASSET);
    let bytes = std::fs::read(&path)
        .map_err(|e| format!("{} could not be read: {e}", path.display()))?;
    decode(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// Splits the blob into its header and its pixels.
///
/// Separate from the file read so the length arithmetic is testable: a truncated asset must be
/// reported, not turned into a `CreateDIBSection` call that reads past the end of a buffer.
pub fn decode(bytes: &[u8]) -> Result<TrayImage, String> {
    if bytes.len() < HEADER {
        return Err("shorter than its 8-byte header".to_owned());
    }
    let width = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let height = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| format!("{width}x{height} is not a believable icon size"))?;
    let pixels = &bytes[HEADER..];
    if pixels.len() != expected {
        return Err(format!(
            "says it is {width}x{height}, which needs {expected} bytes, but carries {}",
            pixels.len()
        ));
    }
    let image = TrayImage { width, height, rgba: pixels.to_vec() };
    if !image.is_valid() {
        return Err("the header describes an empty image".to_owned());
    }
    Ok(image)
}

/// The assets directory: beside the executable when installed, and the repository's own when
/// running from `target/`.
///
/// Deliberately the same two-step lookup as `ghub-ui`'s, because the two binaries are installed
/// into one directory and share one `assets` tree. The fallback is what lets `cargo run -p
/// ghub-agent` work without a staging step.
fn asset_root() -> std::path::PathBuf {
    let beside_exe =
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("assets")));
    if let Some(directory) = beside_exe.filter(|d| d.is_dir()) {
        return directory;
    }
    std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_g_mark_loads_as_a_square_mask() {
        let image = tray_image().expect("the tray asset loads");
        assert_eq!(image.width, image.height, "the G mark is square");
        assert!(image.is_valid());
        // White with a varying alpha: the notification area is dark in both Windows themes'
        // defaults, and the extracted mark is a coverage mask with no colour of its own.
        assert!(image.rgba.chunks(4).all(|p| p[0] == 255 && p[1] == 255 && p[2] == 255));
        assert!(image.rgba.chunks(4).any(|p| p[3] > 0), "the mask is entirely transparent");
    }

    /// A truncated or mislabelled asset has to be reported. Without this check the length would be
    /// handed to `CreateDIBSection`, which would read past the end of the buffer.
    #[test]
    fn a_blob_that_disagrees_with_its_header_is_refused() {
        assert!(decode(&[]).is_err());
        assert!(decode(&[1, 0, 0, 0, 1, 0, 0, 0]).is_err(), "header with no pixels");
        let mut good = Vec::new();
        good.extend_from_slice(&1u32.to_le_bytes());
        good.extend_from_slice(&1u32.to_le_bytes());
        good.extend_from_slice(&[255, 255, 255, 128]);
        assert_eq!(decode(&good).expect("a one-pixel icon").rgba, vec![255, 255, 255, 128]);
        good.push(0);
        assert!(decode(&good).is_err(), "one byte too many");
    }

    /// `width * height * 4` must not be computed in a way that wraps: a hostile or corrupt header
    /// would otherwise pass the length check with a tiny buffer.
    #[test]
    fn an_absurd_header_cannot_overflow_the_length_check() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&u32::MAX.to_le_bytes());
        blob.extend_from_slice(&u32::MAX.to_le_bytes());
        blob.extend_from_slice(&[0; 16]);
        assert!(decode(&blob).is_err());
    }
}
