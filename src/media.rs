use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use url::Url;

use crate::feed::ContentSource;

pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

pub fn download_cover(source: &dyn ContentSource, url: &Url) -> Result<String, String> {
    if !crate::render::is_public_https_url(url.as_str()) {
        return Err("封面 URL 不是允许的公网 HTTPS 地址".to_string());
    }
    let bytes = source.fetch_image(url)?;
    if bytes.is_empty() {
        return Err("封面响应为空".to_string());
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!("封面超过 {MAX_IMAGE_BYTES} 字节限制"));
    }
    let kind = infer::get(&bytes).ok_or_else(|| "无法识别封面格式".to_string())?;
    if !matches!(
        kind.mime_type(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return Err(format!("不支持的封面格式：{}", kind.mime_type()));
    }
    Ok(STANDARD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::ContentSource;

    struct Source(Vec<u8>);

    impl ContentSource for Source {
        fn fetch_rss(&self, _url: &Url) -> Result<Vec<u8>, String> {
            unreachable!()
        }

        fn fetch_markdown(&self, _url: &Url) -> Result<String, String> {
            unreachable!()
        }

        fn fetch_image(&self, _url: &Url) -> Result<Vec<u8>, String> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn encodes_valid_png() {
        let png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];
        let encoded = download_cover(
            &Source(png),
            &Url::parse("https://assets.juya.uk/cover.png").unwrap(),
        )
        .unwrap();
        assert!(!encoded.is_empty());
    }

    #[test]
    fn rejects_private_url() {
        let error = download_cover(
            &Source(vec![1, 2, 3]),
            &Url::parse("https://127.0.0.1/cover.png").unwrap(),
        )
        .unwrap_err();
        assert!(error.contains("公网 HTTPS"));
    }
}
