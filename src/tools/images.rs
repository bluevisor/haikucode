//! Images: path detection and base64 data URIs for view_image and @attachments.

use super::expand;
use std::path::Path;

// ------------------------------------------------------------- images -------

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];
const IMAGE_MAX_BYTES: usize = 5_000_000;

pub fn is_image_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Read an image file and return a `data:image/...;base64,...` URI.
pub fn image_data_uri(path: &str) -> std::result::Result<String, String> {
    let p = expand(path);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !IMAGE_EXTS.contains(&ext.as_str()) {
        return Err(format!("ERROR: {path} is not a supported image (png/jpg/gif/webp)"));
    }
    let bytes = std::fs::read(&p).map_err(|e| format!("ERROR: {e}"))?;
    if bytes.len() > IMAGE_MAX_BYTES {
        return Err(format!("ERROR: {path} is too large ({} bytes; max 5MB)", bytes.len()));
    }
    let mime = if ext == "jpg" { "jpeg".to_string() } else { ext };
    Ok(format!("data:image/{mime};base64,{}", base64_encode(&bytes)))
}

/// Standard base64 (no line wrapping). Kept tiny so we add no dependency.
pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // A non-UTF8 byte sequence (real image data isn't text).
        assert_eq!(base64_encode(&[0xff, 0xd8, 0xff]), "/9j/");
    }

    #[test]
    fn image_path_detection_and_uri() {
        assert!(is_image_path("shot.PNG"));
        assert!(is_image_path("a/b/diagram.jpeg"));
        assert!(!is_image_path("notes.txt"));
        assert!(!is_image_path("Makefile"));
        // jpg maps to the image/jpeg mime.
        let dir = std::env::temp_dir().join(format!("haikucode_img_test-{}.jpg", std::process::id()));
        std::fs::write(&dir, [0xff, 0xd8, 0xff, 0xe0]).unwrap();
        let uri = image_data_uri(dir.to_str().unwrap()).unwrap();
        assert!(uri.starts_with("data:image/jpeg;base64,/9j/"), "got: {uri}");
        std::fs::remove_file(&dir).ok();
        assert!(image_data_uri("nope.png").unwrap_err().starts_with("ERROR"));
    }
}
