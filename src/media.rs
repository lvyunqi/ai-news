use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use url::Url;

use crate::feed::ContentSource;

pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

pub fn download_cover(source: &dyn ContentSource, url: &Url) -> Result<String, String> {
    if !crate::render::is_public_https_url(url.as_str()) {
        return Err("封面 URL 不是允许的公网 HTTPS 地址".to_string());
    }
    let response = source.fetch_image(url)?;
    let bytes = response.bytes;
    if bytes.is_empty() {
        return Err("封面响应为空".to_string());
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!("封面超过 {MAX_IMAGE_BYTES} 字节限制"));
    }
    let kind = infer::get(&bytes).ok_or_else(|| "无法识别封面格式".to_string())?;
    let detected_mime = kind.mime_type();
    if !matches!(
        detected_mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return Err(format!("不支持的封面格式：{detected_mime}"));
    }
    let declared_mime = response
        .content_type
        .ok_or_else(|| "封面响应缺少 Content-Type".to_string())?;
    if declared_mime != detected_mime {
        return Err(format!(
            "封面 Content-Type 与文件内容不一致：{declared_mime} != {detected_mime}"
        ));
    }
    Ok(STANDARD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::{ContentSource, ImageResponse};

    struct Source {
        bytes: Vec<u8>,
        content_type: Option<String>,
    }

    impl ContentSource for Source {
        fn fetch_rss(&self, _url: &Url) -> Result<Vec<u8>, String> {
            unreachable!()
        }

        fn fetch_markdown(&self, _url: &Url) -> Result<String, String> {
            unreachable!()
        }

        fn fetch_image(&self, _url: &Url) -> Result<ImageResponse, String> {
            Ok(ImageResponse {
                bytes: self.bytes.clone(),
                content_type: self.content_type.clone(),
            })
        }
    }

    fn source(bytes: Vec<u8>, content_type: &str) -> Source {
        Source {
            bytes,
            content_type: Some(content_type.to_string()),
        }
    }

    #[test]
    fn encodes_valid_png() {
        let png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];
        let encoded = download_cover(
            &source(png, "image/png"),
            &Url::parse("https://assets.juya.uk/cover.png").unwrap(),
        )
        .unwrap();
        assert!(!encoded.is_empty());
    }

    #[test]
    fn rejects_private_url() {
        let error = download_cover(
            &source(vec![1, 2, 3], "application/octet-stream"),
            &Url::parse("https://127.0.0.1/cover.png").unwrap(),
        )
        .unwrap_err();
        assert!(error.contains("公网 HTTPS"));
    }

    #[test]
    fn accepts_supported_image_formats() {
        let fixtures = [
            (
                vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0],
                "image/png",
            ),
            (vec![0xff, 0xd8, 0xff, 0xe0, 0, 0, 0, 0], "image/jpeg"),
            (b"GIF89a\0\0\0\0".to_vec(), "image/gif"),
            (b"RIFF\x04\0\0\0WEBPVP8 ".to_vec(), "image/webp"),
        ];

        for (bytes, mime) in fixtures {
            let encoded = download_cover(
                &source(bytes, mime),
                &Url::parse("https://assets.juya.uk/cover").unwrap(),
            )
            .unwrap();
            assert!(!encoded.is_empty(), "{mime}");
        }
    }

    #[test]
    fn rejects_mime_and_magic_mismatch() {
        let png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];
        let error = download_cover(
            &source(png, "image/jpeg"),
            &Url::parse("https://assets.juya.uk/cover.png").unwrap(),
        )
        .unwrap_err();

        assert!(error.contains("不一致"));
    }

    #[test]
    fn rejects_missing_content_type() {
        let png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];
        let error = download_cover(
            &Source {
                bytes: png,
                content_type: None,
            },
            &Url::parse("https://assets.juya.uk/cover.png").unwrap(),
        )
        .unwrap_err();

        assert!(error.contains("Content-Type"));
    }

    #[test]
    fn rejects_oversized_image() {
        let error = download_cover(
            &source(vec![0; MAX_IMAGE_BYTES + 1], "image/png"),
            &Url::parse("https://assets.juya.uk/cover.png").unwrap(),
        )
        .unwrap_err();

        assert!(error.contains("超过"));
    }
}
