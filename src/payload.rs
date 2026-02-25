use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClipboardPayload {
    Text(String),
    Image {
        width: u32,
        height: u32,
        png_data: Vec<u8>,
    },
    Files(Vec<FileEntry>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub data: Vec<u8>,
}

impl ClipboardPayload {
    pub fn serialize(&self) -> Result<Vec<u8>> {
        bincode::serialize(self).with_context(|| "Failed to serialize clipboard payload")
    }

    pub fn deserialize(data: &[u8]) -> Result<Self> {
        bincode::deserialize(data).with_context(|| "Failed to deserialize clipboard payload")
    }

    pub fn content_type_str(&self) -> &'static str {
        match self {
            ClipboardPayload::Text(_) => "text",
            ClipboardPayload::Image { .. } => "image",
            ClipboardPayload::Files(_) => "files",
        }
    }
}

/// Convert raw RGBA pixel data to PNG bytes.
pub fn rgba_to_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let img = image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .with_context(|| "Invalid RGBA data dimensions")?;
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png)
        .with_context(|| "Failed to encode PNG")?;
    Ok(buf.into_inner())
}

/// Convert PNG bytes back to raw RGBA pixel data, returning (width, height, rgba_bytes).
pub fn png_to_rgba(png_data: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory_with_format(png_data, image::ImageFormat::Png)
        .with_context(|| "Failed to decode PNG")?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok((width, height, rgba.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_deserialize_text() {
        let payload = ClipboardPayload::Text("hello world".to_string());
        let data = payload.serialize().unwrap();
        let recovered = ClipboardPayload::deserialize(&data).unwrap();
        match recovered {
            ClipboardPayload::Text(s) => assert_eq!(s, "hello world"),
            _ => panic!("Expected Text variant"),
        }
    }

    #[test]
    fn serialize_deserialize_image() {
        let payload = ClipboardPayload::Image {
            width: 2,
            height: 2,
            png_data: vec![1, 2, 3, 4],
        };
        let data = payload.serialize().unwrap();
        let recovered = ClipboardPayload::deserialize(&data).unwrap();
        match recovered {
            ClipboardPayload::Image {
                width,
                height,
                png_data,
            } => {
                assert_eq!(width, 2);
                assert_eq!(height, 2);
                assert_eq!(png_data, vec![1, 2, 3, 4]);
            }
            _ => panic!("Expected Image variant"),
        }
    }

    #[test]
    fn serialize_deserialize_files() {
        let payload = ClipboardPayload::Files(vec![
            FileEntry {
                name: "test.txt".to_string(),
                data: b"content".to_vec(),
            },
            FileEntry {
                name: "other.bin".to_string(),
                data: vec![0xFF, 0x00],
            },
        ]);
        let data = payload.serialize().unwrap();
        let recovered = ClipboardPayload::deserialize(&data).unwrap();
        match recovered {
            ClipboardPayload::Files(files) => {
                assert_eq!(files.len(), 2);
                assert_eq!(files[0].name, "test.txt");
                assert_eq!(files[0].data, b"content");
                assert_eq!(files[1].name, "other.bin");
                assert_eq!(files[1].data, vec![0xFF, 0x00]);
            }
            _ => panic!("Expected Files variant"),
        }
    }

    #[test]
    fn rgba_to_png_to_rgba_round_trip() {
        let width = 4u32;
        let height = 4u32;
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                rgba.push((x * 64) as u8); // R
                rgba.push((y * 64) as u8); // G
                rgba.push(128); // B
                rgba.push(255); // A
            }
        }

        let png_data = rgba_to_png(&rgba, width, height).unwrap();
        assert!(!png_data.is_empty());

        let (w, h, recovered_rgba) = png_to_rgba(&png_data).unwrap();
        assert_eq!(w, width);
        assert_eq!(h, height);
        assert_eq!(recovered_rgba, rgba);
    }

    #[test]
    fn content_type_str() {
        assert_eq!(
            ClipboardPayload::Text("".to_string()).content_type_str(),
            "text"
        );
        assert_eq!(
            ClipboardPayload::Image {
                width: 0,
                height: 0,
                png_data: vec![]
            }
            .content_type_str(),
            "image"
        );
        assert_eq!(
            ClipboardPayload::Files(vec![]).content_type_str(),
            "files"
        );
    }
}
