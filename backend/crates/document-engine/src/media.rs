//! Repository assets are verified before decoding; display pixels are re-encoded
//! without copying source metadata. Never fetch a URL from document content.
use image::{ImageDecoder, ImageEncoder, ImageFormat, ImageReader, Limits};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Write};

pub const MAX_ASSET_BYTES: usize = 5 * 1024 * 1024;
const MAX_DISPLAY_BYTES: usize = 16 * 1024 * 1024;

pub struct DisplayImage {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub fn valid_path(path: &str) -> bool {
    crate::markdown::allowed_destination(path, true)
}

/// The path hashes the original bytes. The response is a bounded PNG display
/// derivative; originals are never returned by this function.
pub fn display(path: &str, bytes: &[u8]) -> Result<DisplayImage, &'static str> {
    if !valid_path(path) {
        return Err("INVALID_ASSET_PATH");
    }
    if bytes.len() > MAX_ASSET_BYTES {
        return Err("FILE_TOO_LARGE");
    }
    let (hash, extension) = path[7..].rsplit_once('.').ok_or("INVALID_ASSET_PATH")?;
    if format!("{:x}", Sha256::digest(bytes)) != hash {
        return Err("ASSET_HASH_MISMATCH");
    }
    let format = image::guess_format(bytes).map_err(|_| "UNSUPPORTED_MEDIA_TYPE")?;
    if !matches!(
        (format, extension),
        (ImageFormat::Png, "png")
            | (ImageFormat::Jpeg, "jpg" | "jpeg")
            | (ImageFormat::WebP, "webp")
    ) {
        return Err("UNSUPPORTED_MEDIA_TYPE");
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|_| "INVALID_IMAGE")?;
    let (width, height) = decoder.dimensions();
    let pixels = u64::from(width) * u64::from(height);
    if width == 0
        || height == 0
        || pixels > 16 * 1024 * 1024
        || pixels * u64::from(decoder.color_type().bytes_per_pixel()) > 64 * 1024 * 1024
    {
        return Err("IMAGE_DIMENSIONS_EXCEEDED");
    }
    let orientation = decoder.orientation().map_err(|_| "INVALID_IMAGE")?;
    let mut decoded = image::DynamicImage::from_decoder(decoder).map_err(|_| "INVALID_IMAGE")?;
    decoded.apply_orientation(orientation);
    let display = if decoded.width() > 2048 || decoded.height() > 2048 {
        decoded.resize(2048, 2048, image::imageops::FilterType::Triangle)
    } else {
        decoded
    };
    let pixels = display.into_rgba8();
    let mut output = BoundedOutput(Vec::new());
    image::codecs::png::PngEncoder::new(&mut output)
        .write_image(
            pixels.as_raw(),
            pixels.width(),
            pixels.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|_| "IMAGE_ENCODING_FAILED")?;
    Ok(DisplayImage {
        png: output.0,
        width: pixels.width(),
        height: pixels.height(),
    })
}

struct BoundedOutput(Vec<u8>);
impl Write for BoundedOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_DISPLAY_BYTES {
            return Err(std::io::Error::other("display image exceeds limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn path(bytes: &[u8], extension: &str) -> String {
        format!("assets/{:x}.{extension}", Sha256::digest(bytes))
    }
    fn image(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
        let pixels = image::DynamicImage::new_rgb8(width, height);
        let mut bytes = Cursor::new(Vec::new());
        pixels.write_to(&mut bytes, format).unwrap();
        bytes.into_inner()
    }

    #[test]
    fn accepts_only_matching_hashed_raster_content_and_generates_png_display_pixels() {
        for (format, extension) in [
            (ImageFormat::Png, "png"),
            (ImageFormat::Jpeg, "jpg"),
            (ImageFormat::WebP, "webp"),
        ] {
            let bytes = image(format, 8, 4);
            let result = display(&path(&bytes, extension), &bytes).unwrap();
            assert_eq!((result.width, result.height), (8, 4));
            assert_eq!(image::guess_format(&result.png).unwrap(), ImageFormat::Png);
            assert!(image::load_from_memory(&result.png).is_ok());
        }
        let bytes = image(ImageFormat::Png, 8, 4);
        assert!(matches!(
            display(&path(&bytes, "jpg"), &bytes),
            Err("UNSUPPORTED_MEDIA_TYPE")
        ));
        assert!(matches!(
            display(&path(b"other", "png"), &bytes),
            Err("ASSET_HASH_MISMATCH")
        ));
        assert!(matches!(
            display(&path(b"<svg/>", "png"), b"<svg/>"),
            Err("UNSUPPORTED_MEDIA_TYPE")
        ));
        assert!(!valid_path("assets/../secret.png"));
        assert!(!valid_path("https://example.org/image.png"));
    }

    #[test]
    fn honors_exif_orientation_and_strips_source_metadata_from_display_output() {
        // Little-endian TIFF IFD: orientation SHORT=6 (90 degrees clockwise),
        // followed by a synthetic private marker outside the IFD.
        let mut exif = b"II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0".to_vec();
        let marker = b"SYNTHETIC_PRIVATE_LOCATION";
        exif.extend_from_slice(marker);
        let pixels = image::RgbImage::new(8, 4);
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        encoder.set_exif_metadata(exif).unwrap();
        encoder
            .write_image(pixels.as_raw(), 8, 4, image::ExtendedColorType::Rgb8)
            .unwrap();
        assert!(bytes.windows(marker.len()).any(|part| part == marker));
        let result = display(&path(&bytes, "png"), &bytes).unwrap();
        assert_eq!((result.width, result.height), (4, 8));
        assert!(!result.png.windows(marker.len()).any(|part| part == marker));
        assert!(
            !result
                .png
                .windows(4)
                .any(|part| part == b"eXIf" || part == b"iCCP")
        );
    }

    #[test]
    fn book_fixture_asset_is_hashed_and_decodes_as_a_valid_display_image() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../fixtures/repository/assets");
        let path = std::fs::read_dir(root)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let bytes = std::fs::read(&path).unwrap();
        let reference = format!("assets/{}", path.file_name().unwrap().to_str().unwrap());
        let result = display(&reference, &bytes).unwrap();
        assert_eq!((result.width, result.height), (1, 1));
    }

    #[test]
    fn bounds_original_bytes_dimensions_and_display_size() {
        let bytes = vec![0; MAX_ASSET_BYTES + 1];
        assert!(matches!(
            display(&path(&bytes, "png"), &bytes),
            Err("FILE_TOO_LARGE")
        ));
        let bytes = image(ImageFormat::Png, 8193, 1);
        assert!(display(&path(&bytes, "png"), &bytes).is_err());
        let bytes = image(ImageFormat::Png, 4097, 4097);
        assert!(matches!(
            display(&path(&bytes, "png"), &bytes),
            Err("IMAGE_DIMENSIONS_EXCEEDED")
        ));
        let bytes = image(ImageFormat::Png, 3000, 100);
        let result = display(&path(&bytes, "png"), &bytes).unwrap();
        assert_eq!(result.width, 2048);
        assert!(result.height > 0 && result.height < 100);
    }
}
