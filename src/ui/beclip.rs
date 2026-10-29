//! Haiku's clipboard image, read out of a flattened BMessage.
//!
//! `clipboard -s FILE` saves the whole system clipboard as a flattened
//! BMessage. An image copied on Haiku (ShowImage, `screenshot -c`) sits in it
//! as an archived BBitmap under `image/bitmap`: raw pixels plus frame, color
//! space and row stride. We pull those out and write a Translation Kit bitmap
//! (`bits`), which `translate` turns into a compressed PNG.

/// The Haiku flattened-message format tag, as its bytes appear on disk.
const MESSAGE_FORMAT_HAIKU: &[u8; 4] = b"HMF1";
const FIELD_FLAG_FIXED_SIZE: u16 = 0x0002;
/// Header: 12 u32 fields, then the 5-slot hash table (MessagePrivate.h).
const HEADER_LEN: usize = 12 * 4 + 5 * 4;
const FIELD_LEN: usize = 24;

const B_RGB32: u32 = 0x0008;
const B_RGBA32: u32 = 0x2008;

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn f32_at(b: &[u8], at: usize) -> Option<f32> {
    u32_at(b, at).map(f32::from_bits)
}

/// The first item of field `name` in a flattened BMessage.
fn find_field<'a>(msg: &'a [u8], name: &str) -> Option<&'a [u8]> {
    if msg.get(..4)? != MESSAGE_FORMAT_HAIKU {
        return None;
    }
    let data_size = u32_at(msg, 36)? as usize;
    let count = u32_at(msg, 40)? as usize;
    let data_start = HEADER_LEN.checked_add(count.checked_mul(FIELD_LEN)?)?;
    let data = msg.get(data_start..data_start.checked_add(data_size)?)?;
    for i in 0..count {
        let f = HEADER_LEN + i * FIELD_LEN;
        let flags = u16::from_le_bytes(msg.get(f..f + 2)?.try_into().ok()?);
        let name_len = u16::from_le_bytes(msg.get(f + 2..f + 4)?.try_into().ok()?) as usize;
        let items = u32_at(msg, f + 8)? as usize;
        let size = u32_at(msg, f + 12)? as usize;
        let offset = u32_at(msg, f + 16)? as usize;
        let field_name = data.get(offset..offset + name_len)?;
        if field_name.strip_suffix(b"\0").unwrap_or(field_name) != name.as_bytes() || items == 0 {
            continue;
        }
        let body = data.get(offset + name_len..offset + name_len + size)?;
        return if flags & FIELD_FLAG_FIXED_SIZE != 0 {
            body.get(..size / items)
        } else {
            // Variable-size items are each prefixed with their u32 length.
            let len = u32_at(body, 0)? as usize;
            body.get(4..4 + len)
        };
    }
    None
}

/// The clipboard's bitmap as a Translation Kit `bits` file, or None when the
/// clipboard holds no image (or one in a color space we don't handle).
pub fn clipboard_bits(clip: &[u8]) -> Option<Vec<u8>> {
    let bitmap = find_field(clip, "image/bitmap")?;
    let frame = find_field(bitmap, "_frame")?;
    let cspace = u32_at(find_field(bitmap, "_cspace")?, 0)?;
    let row_bytes = u32_at(find_field(bitmap, "_rowbytes")?, 0)?;
    let pixels = find_field(bitmap, "_data")?;
    if cspace != B_RGB32 && cspace != B_RGBA32 {
        return None;
    }
    let (l, t, r, b) = (f32_at(frame, 0)?, f32_at(frame, 4)?, f32_at(frame, 8)?, f32_at(frame, 12)?);
    let rows = (b - t + 1.0) as u32;
    if rows == 0 || r < l || pixels.len() < (row_bytes * rows) as usize {
        return None;
    }
    // TranslatorBitmap: magic, bounds, row bytes, color space, data size,
    // all big-endian.
    let mut out = Vec::with_capacity(32 + pixels.len());
    out.extend_from_slice(b"bits");
    for v in [l, t, r, b] {
        out.extend_from_slice(&v.to_bits().to_be_bytes());
    }
    for v in [row_bytes, cspace, row_bytes * rows] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(&pixels[..(row_bytes * rows) as usize]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flatten a message of (name, type, fixed, item) fields the way
    /// BMessage::Flatten lays them out.
    fn flatten(fields: &[(&str, &[u8; 4], bool, Vec<u8>)]) -> Vec<u8> {
        let mut heads = Vec::new();
        let mut data = Vec::new();
        for (name, ty, fixed, item) in fields {
            let offset = data.len() as u32;
            data.extend_from_slice(name.as_bytes());
            data.push(0);
            let size = if *fixed {
                data.extend_from_slice(item);
                item.len()
            } else {
                data.extend_from_slice(&(item.len() as u32).to_le_bytes());
                data.extend_from_slice(item);
                item.len() + 4
            };
            heads.extend_from_slice(&(if *fixed { 3u16 } else { 1u16 }).to_le_bytes());
            heads.extend_from_slice(&(name.len() as u16 + 1).to_le_bytes());
            heads.extend_from_slice(&u32::from_be_bytes(**ty).to_le_bytes());
            for v in [1u32, size as u32, offset, u32::MAX] {
                heads.extend_from_slice(&v.to_le_bytes());
            }
        }
        let mut out = Vec::new();
        out.extend_from_slice(MESSAGE_FORMAT_HAIKU);
        out.extend_from_slice(&u32::from_be_bytes(*b"DATA").to_le_bytes());
        out.extend_from_slice(&[0xff; 28]);
        for v in [data.len() as u32, fields.len() as u32, 5] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&[0xff; 20]);
        out.extend(heads);
        out.extend(data);
        out
    }

    fn le_f32s(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    #[test]
    fn copied_bitmap_becomes_a_bits_file() {
        let pixels: Vec<u8> = (0..2 * 3 * 4).map(|i| i as u8).collect();
        let bitmap = flatten(&[
            ("class", b"CSTR", false, b"BBitmap\0".to_vec()),
            ("_frame", b"RECT", true, le_f32s(&[0.0, 0.0, 1.0, 2.0])),
            ("_cspace", b"LONG", true, B_RGB32.to_le_bytes().to_vec()),
            ("_bmflags", b"LONG", true, 0u32.to_le_bytes().to_vec()),
            ("_rowbytes", b"LONG", true, 8u32.to_le_bytes().to_vec()),
            ("_data", b"RAWT", false, pixels.clone()),
        ]);
        let clip = flatten(&[("image/bitmap", b"MSGG", false, bitmap)]);
        let bits = clipboard_bits(&clip).expect("bits");
        assert_eq!(&bits[..4], b"bits");
        assert_eq!(f32::from_bits(u32::from_be_bytes(bits[16..20].try_into().unwrap())), 2.0);
        assert_eq!(u32::from_be_bytes(bits[20..24].try_into().unwrap()), 8);
        assert_eq!(u32::from_be_bytes(bits[24..28].try_into().unwrap()), B_RGB32);
        assert_eq!(u32::from_be_bytes(bits[28..32].try_into().unwrap()), 24);
        assert_eq!(&bits[32..], &pixels[..]);
    }

    #[test]
    fn text_clipboard_has_no_image() {
        let clip = flatten(&[("text/plain", b"MIME", false, b"hello".to_vec())]);
        assert!(clipboard_bits(&clip).is_none());
        assert!(clipboard_bits(b"not a message").is_none());
    }
}
