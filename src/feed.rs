use std::io::{Cursor, Read};

use chrono::{DateTime, NaiveDate};
use chrono_tz::Tz;
use regex::Regex;
use reqwest::header::LOCATION;
use rss::{Channel, Item};
use url::Url;

const MAX_RSS_BYTES: usize = 2 * 1024 * 1024;
const MAX_MARKDOWN_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub id: String,
    pub date: NaiveDate,
    pub title: String,
    pub page_url: String,
    pub markdown_url: String,
    pub description: String,
    pub published_timestamp: i64,
}

pub trait ContentSource {
    fn fetch_rss(&self, url: &Url) -> Result<Vec<u8>, String>;
    fn fetch_markdown(&self, url: &Url) -> Result<String, String>;
    fn fetch_image(&self, url: &Url) -> Result<Vec<u8>, String>;
}

pub struct HttpContentSource {
    client: reqwest::blocking::Client,
}

impl HttpContentSource {
    pub fn new(timeout: std::time::Duration) -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(timeout)
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("qimen-ai-news/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| format!("创建 HTTP 客户端失败：{error}"))?;
        Ok(Self { client })
    }

    fn fetch_bytes(
        &self,
        url: &Url,
        limit: usize,
        max_redirects: usize,
        require_public: bool,
    ) -> Result<Vec<u8>, String> {
        if url.scheme() != "https" {
            return Err("请求 URL 必须使用 HTTPS".to_string());
        }
        if require_public && !crate::render::is_public_https_url(url.as_str()) {
            return Err("请求 URL 不是允许的公网 HTTPS 地址".to_string());
        }
        let response = self
            .client
            .get(url.clone())
            .send()
            .map_err(|error| format!("请求失败：{error}"))?;
        if response.status().is_redirection() {
            if max_redirects == 0 {
                return Err("重定向次数超过限制".to_string());
            }
            let location = response
                .headers()
                .get(LOCATION)
                .ok_or_else(|| "重定向响应缺少 Location".to_string())?
                .to_str()
                .map_err(|_| "重定向 Location 不是有效文本".to_string())?;
            let next = url
                .join(location)
                .map_err(|error| format!("重定向 URL 无效：{error}"))?;
            return self.fetch_bytes(&next, limit, max_redirects - 1, require_public);
        }
        let response = response
            .error_for_status()
            .map_err(|error| format!("服务器返回错误状态：{error}"))?;

        if response
            .content_length()
            .is_some_and(|size| size > limit as u64)
        {
            return Err(format!("响应体超过 {} 字节限制", limit));
        }

        let mut bytes = Vec::with_capacity(response.content_length().unwrap_or(0) as usize);
        response
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("读取响应失败：{error}"))?;
        if bytes.len() > limit {
            return Err(format!("响应体超过 {} 字节限制", limit));
        }
        Ok(bytes)
    }
}

impl ContentSource for HttpContentSource {
    fn fetch_rss(&self, url: &Url) -> Result<Vec<u8>, String> {
        self.fetch_bytes(url, MAX_RSS_BYTES, 5, false)
    }

    fn fetch_markdown(&self, url: &Url) -> Result<String, String> {
        let bytes = self.fetch_bytes(url, MAX_MARKDOWN_BYTES, 5, false)?;
        String::from_utf8(bytes).map_err(|error| format!("Markdown 不是 UTF-8：{error}"))
    }

    fn fetch_image(&self, url: &Url) -> Result<Vec<u8>, String> {
        self.fetch_bytes(url, crate::media::MAX_IMAGE_BYTES, 3, true)
    }
}

pub fn parse_latest_today(
    bytes: &[u8],
    today: NaiveDate,
    timezone: Tz,
    feed_url: &Url,
) -> Result<Option<Issue>, String> {
    let channel = Channel::read_from(Cursor::new(bytes))
        .map_err(|error| format!("RSS XML 解析失败：{error}"))?;
    Ok(select_latest_today(&channel, today, timezone, feed_url))
}

fn select_latest_today(
    channel: &Channel,
    today: NaiveDate,
    timezone: Tz,
    feed_url: &Url,
) -> Option<Issue> {
    let mut candidates = channel
        .items()
        .iter()
        .filter_map(|item| issue_from_item(item, timezone, feed_url))
        .filter(|issue| issue.date == today)
        .collect::<Vec<_>>();
    candidates.sort_by_key(|issue| std::cmp::Reverse(issue.published_timestamp));
    candidates.into_iter().next()
}

fn issue_from_item(item: &Item, timezone: Tz, feed_url: &Url) -> Option<Issue> {
    let title = item.title().unwrap_or_default().trim().to_string();
    let page_url = item.link().unwrap_or_default().trim().to_string();
    let id = item
        .guid()
        .map(|guid| guid.value().trim())
        .filter(|value| !value.is_empty())
        .or_else(|| (!page_url.is_empty()).then_some(page_url.as_str()))?
        .to_string();

    let published = item
        .pub_date()
        .and_then(|value| DateTime::parse_from_rfc2822(value).ok());
    let date = parse_title_date(&title)
        .or_else(|| published.map(|value| value.with_timezone(&timezone).date_naive()))?;
    let published_timestamp = published.map_or(0, |value| value.timestamp());
    let description = item.description().unwrap_or_default().trim().to_string();
    let markdown_url = derive_markdown_url(&page_url, feed_url, date)?;

    Some(Issue {
        id,
        date,
        title,
        page_url,
        markdown_url,
        description,
        published_timestamp,
    })
}

fn parse_title_date(title: &str) -> Option<NaiveDate> {
    let regex = Regex::new(r"\b(\d{4}-\d{2}-\d{2})\b").expect("valid date regex");
    let value = regex.captures(title)?.get(1)?.as_str();
    NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
}

fn derive_markdown_url(page_url: &str, feed_url: &Url, date: NaiveDate) -> Option<String> {
    let mut url = Url::parse(page_url)
        .ok()
        .unwrap_or_else(|| feed_url.clone());
    url.set_query(None);
    url.set_fragment(None);
    url.set_path(&format!("/markdown/{}.md", date.format("%Y-%m-%d")));
    Some(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
      <rss version="2.0"><channel><title>AI</title>
        <item><title>2026-08-09</title><link>https://daily.juya.uk/issues/2026-08-09/</link>
          <guid>old</guid><pubDate>Sun, 09 Aug 2026 01:00:00 GMT</pubDate><description>old</description></item>
        <item><title>2026-08-10</title><link>https://daily.juya.uk/issues/2026-08-10/</link>
          <guid>new</guid><pubDate>Mon, 10 Aug 2026 01:30:00 GMT</pubDate><description>new</description></item>
      </channel></rss>"#;

    #[test]
    fn selects_only_today() {
        let feed_url = Url::parse("https://daily.juya.uk/rss.xml").unwrap();
        let issue = parse_latest_today(
            RSS.as_bytes(),
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap(),
            chrono_tz::Asia::Shanghai,
            &feed_url,
        )
        .unwrap()
        .unwrap();

        assert_eq!(issue.id, "new");
        assert_eq!(
            issue.markdown_url,
            "https://daily.juya.uk/markdown/2026-08-10.md"
        );
    }

    #[test]
    fn ignores_historical_items() {
        let feed_url = Url::parse("https://daily.juya.uk/rss.xml").unwrap();
        let issue = parse_latest_today(
            RSS.as_bytes(),
            NaiveDate::from_ymd_opt(2026, 8, 11).unwrap(),
            chrono_tz::Asia::Shanghai,
            &feed_url,
        )
        .unwrap();
        assert!(issue.is_none());
    }

    #[test]
    fn falls_back_to_link_when_guid_is_missing() {
        let rss = r#"<rss version="2.0"><channel><title>AI</title><item>
          <title>2026-08-10</title><link>https://daily.juya.uk/issues/2026-08-10/</link>
          <pubDate>Mon, 10 Aug 2026 01:30:00 GMT</pubDate></item></channel></rss>"#;
        let feed_url = Url::parse("https://daily.juya.uk/rss.xml").unwrap();
        let issue = parse_latest_today(
            rss.as_bytes(),
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap(),
            chrono_tz::Asia::Shanghai,
            &feed_url,
        )
        .unwrap()
        .unwrap();
        assert_eq!(issue.id, issue.page_url);
    }
}
