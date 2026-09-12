//! Deriving a master secret from a photograph.
//!
//! The secret comes from DECODED pixels, never from file bytes, so re-tagging or
//! re-containering an image does not change the wallet. Re-encoding it does --
//! a photo is not a backup, and the UI must say so.

use std::collections::HashSet;
use std::path::Path;

use zeroize::Zeroize;

use crate::seed::MasterSeed;

/// Photo-to-master derivation. NEVER change this string.
///
/// Deliberately different from the parano1d wallet's equivalent: people reuse
/// photographs, and one leak must not empty wallets on two chains.
pub const PHOTO_CONTEXT: &str = "alphanumeric master secret from canonical image pixels v1";

const MAX_PHOTO_BYTES: u64 = 256 << 20;
const MIN_DIMENSION: u32 = 32;
// One-way door: this may only ever be LOWERED. Raising it would lock out anyone
// who already funded a wallet derived from a photo that met the old, lower bar.
const MIN_DISTINCT_PIXELS: usize = 64;

pub struct PhotoSecret {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub key_id: String,
    master: MasterSeed,
}

impl std::fmt::Debug for PhotoSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhotoSecret")
            .field("name", &self.name)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("key_id", &self.key_id)
            .field("master", &"<redacted>")
            .finish()
    }
}

impl PhotoSecret {
    pub fn master(&self) -> MasterSeed {
        self.master.clone()
    }
}

pub fn prepare_secret_photo(path: &Path) -> Result<PhotoSecret, String> {
    let metadata = std::fs::metadata(path).map_err(|e| format!("Read {}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err("Choose a photo.".into());
    }
    if metadata.len() == 0 {
        return Err("The selected photo is empty.".into());
    }
    if metadata.len() > MAX_PHOTO_BYTES {
        return Err("The selected photo is larger than 256 MiB.".into());
    }

    let decoded = image::ImageReader::open(path)
        .map_err(|e| format!("Unsupported or unreadable image: {e}"))?
        .with_guessed_format()
        .map_err(|e| format!("Unsupported or unreadable image: {e}"))?
        .decode()
        .map_err(|e| format!("Unsupported or unreadable image: {e}"))?;

    let width = decoded.width();
    let height = decoded.height();
    let rgba = decoded.to_rgba8();
    validate_source(width, height, rgba.as_raw())?;

    let mut secret = derive(width, height, rgba.as_raw());

    // Wipe BOTH pixel copies. to_rgba8() on a non-RGBA source -- every JPEG --
    // allocates a second buffer and leaves the decoder's own one behind, so
    // wiping only the RGBA copy would be half a measure: JPEG is the format
    // people actually choose. The usual argument for skipping this ("the photo
    // is on disk anyway") fails exactly when it matters, because a user who
    // learns the photo is their key may then delete it -- at which point freed
    // heap is the only copy left.
    //
    // ImageBuffer has no as_mut() in this version, so both go through into_raw().
    let mut pixels = rgba.into_raw();
    pixels.zeroize();
    wipe_decoded(decoded);

    let master = MasterSeed::from_bytes(secret);
    secret.zeroize();

    Ok(PhotoSecret {
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        width,
        height,
        key_id: master.key_id(),
        master,
    })
}

/// Zeroize a decoded image's native pixel buffer.
///
/// `DynamicImage` owns its buffer, so this needs no cooperation from the decoder
/// -- just a match over the variants. Dropping it would return the allocation
/// without clearing it.
fn wipe_decoded(decoded: image::DynamicImage) {
    use image::DynamicImage::*;
    match decoded {
        ImageLuma8(buffer) => buffer.into_raw().zeroize(),
        ImageLumaA8(buffer) => buffer.into_raw().zeroize(),
        ImageRgb8(buffer) => buffer.into_raw().zeroize(),
        ImageRgba8(buffer) => buffer.into_raw().zeroize(),
        ImageLuma16(buffer) => buffer.into_raw().zeroize(),
        ImageLumaA16(buffer) => buffer.into_raw().zeroize(),
        ImageRgb16(buffer) => buffer.into_raw().zeroize(),
        ImageRgba16(buffer) => buffer.into_raw().zeroize(),
        ImageRgb32F(buffer) => buffer.into_raw().zeroize(),
        ImageRgba32F(buffer) => buffer.into_raw().zeroize(),
        // DynamicImage is #[non_exhaustive]; a variant added later is not wiped,
        // which is worth knowing rather than silently accepting. Deliberately NOT
        // formatting `other`: DynamicImage's derived Debug includes the pixel
        // buffer, so printing it here would dump the decoded image -- the key --
        // to stderr in a debug build.
        _ => debug_assert!(
            false,
            "unhandled DynamicImage variant -- its buffer was not wiped"
        ),
    }
}

fn validate_source(width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    if width < MIN_DIMENSION || height < MIN_DIMENSION {
        return Err(format!(
            "The image must be at least {MIN_DIMENSION} x {MIN_DIMENSION} pixels."
        ));
    }

    let mut distinct = HashSet::with_capacity(MIN_DISTINCT_PIXELS);
    for pixel in rgba.chunks_exact(4) {
        distinct.insert([pixel[0], pixel[1], pixel[2], pixel[3]]);
        if distinct.len() >= MIN_DISTINCT_PIXELS {
            return Ok(());
        }
    }
    Err(format!(
        "The image must contain at least {MIN_DISTINCT_PIXELS} distinct pixel values."
    ))
}

fn derive(width: u32, height: u32, rgba: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(PHOTO_CONTEXT);
    hasher.update(b"RGBA8");
    hasher.update(&width.to_le_bytes());
    hasher.update(&height.to_le_bytes());
    hasher.update(&(rgba.len() as u64).to_le_bytes());
    hasher.update(rgba);
    let derived = *hasher.finalize().as_bytes();
    // blake3's zeroize feature gives the Hasher a zeroize() METHOD, not
    // ZeroizeOnDrop -- dropping it here would leave the master in its chunk
    // buffer unless this is called explicitly.
    hasher.zeroize();
    derived
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    use image::{ExtendedColorType, ImageEncoder};
    use std::fs::File;

    fn gradient(size: u32) -> Vec<u8> {
        (0..size * size)
            .flat_map(|index| {
                let x = (index % size) as u8;
                let y = (index / size) as u8;
                [
                    x.wrapping_mul(7),
                    y.wrapping_mul(5),
                    x.wrapping_mul(11) ^ y,
                    0xff,
                ]
            })
            .collect()
    }

    // The secret comes from decoded pixels, not from file bytes. Container
    // metadata must not change the wallet: two files with identical pixels and
    // different EXIF are the same photo.
    #[test]
    fn container_metadata_does_not_change_the_secret() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plain = dir.path().join("plain.png");
        let tagged = dir.path().join("tagged.png");
        let pixels = gradient(32);

        PngEncoder::new(File::create(&plain).expect("create"))
            .write_image(&pixels, 32, 32, ExtendedColorType::Rgba8)
            .expect("encode");
        let mut encoder = PngEncoder::new(File::create(&tagged).expect("create"));
        encoder
            .set_exif_metadata(b"private metadata that must not become a key".to_vec())
            .expect("exif");
        encoder
            .write_image(&pixels, 32, 32, ExtendedColorType::Rgba8)
            .expect("encode");

        assert_ne!(
            std::fs::read(&plain).expect("read"),
            std::fs::read(&tagged).expect("read"),
            "the two files must differ on disk for this test to mean anything"
        );

        let a = prepare_secret_photo(&plain).expect("plain photo");
        let b = prepare_secret_photo(&tagged).expect("tagged photo");
        assert_eq!(a.key_id, b.key_id);
        assert_eq!(
            a.master().child_seed(0).as_slice(),
            b.master().child_seed(0).as_slice()
        );
    }

    // Without this, a derive() that hashed only (width, height) and ignored the
    // pixels entirely would pass every other test in this file: the EXIF test
    // uses identical pixels on both sides, the golden pins one fixed image, and
    // the rest only exercise rejections.
    #[test]
    fn different_pixels_give_a_different_wallet() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("first.png");
        let second = dir.path().join("second.png");

        let mut pixels = gradient(32);
        PngEncoder::new(File::create(&first).expect("create"))
            .write_image(&pixels, 32, 32, ExtendedColorType::Rgba8)
            .expect("encode");

        // One channel of one pixel, same dimensions and same buffer length.
        pixels[0] ^= 0x01;
        PngEncoder::new(File::create(&second).expect("create"))
            .write_image(&pixels, 32, 32, ExtendedColorType::Rgba8)
            .expect("encode");

        let a = prepare_secret_photo(&first).expect("first photo");
        let b = prepare_secret_photo(&second).expect("second photo");
        assert_eq!((a.width, a.height), (b.width, b.height));
        assert_ne!(
            a.key_id, b.key_id,
            "a one-bit pixel change must open a different wallet"
        );
    }

    // JPEG decoding is decoder-dependent. This vector is why `image` carries an
    // EXACT pin: if a version bump changes a single decoded pixel, the same photo
    // stops opening the same wallet.
    #[test]
    fn jpeg_has_a_cross_platform_golden_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("photo.jpg");
        let pixels: Vec<u8> = (0..32 * 32)
            .flat_map(|index| {
                let x = (index % 32) as u8;
                let y = (index / 32) as u8;
                [
                    x.wrapping_mul(7),
                    y.wrapping_mul(5),
                    x.wrapping_mul(y).wrapping_mul(3),
                ]
            })
            .collect();
        JpegEncoder::new_with_quality(File::create(&path).expect("create"), 83)
            .write_image(&pixels, 32, 32, ExtendedColorType::Rgb8)
            .expect("encode");

        let photo = prepare_secret_photo(&path).expect("jpeg photo");
        assert_eq!(photo.key_id, JPEG_GOLDEN_KEY_ID);
    }

    // Rejected sources: too small, or too uniform to carry entropy. A wallet
    // derived from a blank image is a wallet anyone can derive.
    #[test]
    fn tiny_and_uniform_sources_are_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");

        let tiny = dir.path().join("tiny.png");
        PngEncoder::new(File::create(&tiny).expect("create"))
            .write_image(&[0xff, 0, 0, 0xff], 1, 1, ExtendedColorType::Rgba8)
            .expect("encode");
        assert!(prepare_secret_photo(&tiny)
            .expect_err("1x1 must be refused")
            .contains("32"));

        let uniform = dir.path().join("uniform.png");
        let flat = [0x22u8, 0x44, 0x66, 0xff].repeat(32 * 32);
        PngEncoder::new(File::create(&uniform).expect("create"))
            .write_image(&flat, 32, 32, ExtendedColorType::Rgba8)
            .expect("encode");
        assert!(prepare_secret_photo(&uniform)
            .expect_err("a flat image must be refused")
            .contains("distinct"));
    }

    #[test]
    fn a_non_image_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"this must never become a wallet secret").expect("write");
        assert!(prepare_secret_photo(&path).is_err());
    }

    // Deliberate divergence from the parano1d wallet's context string. People
    // reuse photos; if the same photo produced the same key on two chains, one
    // leak would empty both.
    #[test]
    fn context_differs_from_the_parano1d_wallet() {
        assert_ne!(
            PHOTO_CONTEXT,
            "ParanO(1)d master secret from canonical image pixels v1"
        );
        assert!(PHOTO_CONTEXT.starts_with("alphanumeric "));
    }

    /// Pinned fingerprint of the golden JPEG. Regenerating this means decoded
    /// pixels moved, which silently changes which wallet a photo opens.
    const JPEG_GOLDEN_KEY_ID: &str = "4a9b\u{b7}e234\u{b7}34bb\u{b7}4611";
}
