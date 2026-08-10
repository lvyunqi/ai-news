use std::sync::LazyLock;

use regex::{Captures, Regex};
use url::Url;

use crate::feed::Issue;

const ONEBOT_MAX_CHARS: usize = 3500;
const QQ_MARKDOWN_MAX_CHARS: usize = 12_000;

static LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").expect("valid link regex"));
static IMAGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[([^\]]*)\]\((https?://[^)]+)\)").expect("valid image regex"));
static HTML_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<[^>]+>").expect("valid html regex"));
static CODE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`([^`]*)`").expect("valid code regex"));
static ISSUE_NUMBER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*#\d+\s*$").expect("valid issue number regex"));

#[derive(Clone, Debug)]
pub struct PreparedContent {
    pub onebot_text: String,
    pub qq_markdown: String,
    pub cover_url: Option<Url>,
}

pub fn prepare(issue: &Issue, markdown: Option<&str>) -> PreparedContent {
    let fallback = clean_description(&issue.description);
    let onebot_text = render_onebot(issue, markdown, &fallback);
    let qq_markdown = render_qq_markdown(issue, markdown, &fallback);
    let cover_url = markdown.and_then(first_public_image);
    PreparedContent {
        onebot_text,
        qq_markdown,
        cover_url,
    }
}

fn render_onebot(issue: &Issue, markdown: Option<&str>, fallback: &str) -> String {
    let mut body = markdown
        .and_then(extract_plain_overview)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_string());
    if body.trim().is_empty() {
        body = "今日早报已发布。".to_string();
    }

    let page_url = full_page_url(issue);
    let header = format!("【AI 早报 {}】\n\n", issue.date.format("%Y-%m-%d"));
    let footer = format!("\n\n查看全文：{page_url}\n内容由 AI 辅助创作，请注意核实。");
    let available = ONEBOT_MAX_CHARS.saturating_sub(char_count(&header) + char_count(&footer));
    body = truncate_at_line(&body, available);
    format!("{header}{}{footer}", body.trim())
}

fn extract_plain_overview(markdown: &str) -> Option<String> {
    let mut in_overview = false;
    let mut output = Vec::new();
    for raw_line in markdown.lines() {
        let line = raw_line.trim();
        if line == "## 概览" {
            in_overview = true;
            continue;
        }
        if !in_overview {
            continue;
        }
        if line == "---" || line == "***" {
            break;
        }
        if let Some(category) = line.strip_prefix("### ") {
            if !output.is_empty()
                && output
                    .last()
                    .is_some_and(|value: &String| !value.is_empty())
            {
                output.push(String::new());
            }
            output.push(clean_inline(category));
            continue;
        }
        let Some(item) = line.strip_prefix("- ") else {
            continue;
        };

        let mut urls = Vec::new();
        let visible = LINK_RE
            .replace_all(item, |captures: &Captures<'_>| {
                let label = captures.get(1).map_or("", |value| value.as_str());
                let url = captures.get(2).map_or("", |value| value.as_str());
                if is_http_url(url) {
                    urls.push(url.to_string());
                }
                if label == "↗" {
                    String::new()
                } else {
                    label.to_string()
                }
            })
            .to_string();
        let visible = ISSUE_NUMBER_RE
            .replace(&clean_inline(&visible), "")
            .trim()
            .to_string();
        if !visible.is_empty() {
            output.push(format!("- {visible}"));
        }
        urls.sort();
        urls.dedup();
        for url in urls {
            output.push(format!("  {url}"));
        }
    }

    (!output.is_empty()).then(|| output.join("\n"))
}

fn render_qq_markdown(issue: &Issue, markdown: Option<&str>, fallback: &str) -> String {
    let source = markdown.unwrap_or(fallback);
    let normalized = normalize_qq_markdown(source);
    let page_url = full_page_url(issue);
    let result = if normalized.trim().is_empty() {
        format!(
            "# AI 早报 {}\n\n今日早报已发布。\n\n[查看网页全文]({page_url})",
            issue.date.format("%Y-%m-%d")
        )
    } else {
        normalized
    };

    if char_count(&result) <= QQ_MARKDOWN_MAX_CHARS {
        return result;
    }

    let overview = markdown
        .and_then(extract_markdown_overview)
        .unwrap_or_else(|| truncate_at_line(fallback, 8000));
    let compact = format!(
        "# AI 早报 {}\n\n{}\n\n[查看网页全文]({page_url})\n\n**提示**：内容由 AI 辅助创作，可能存在幻觉和错误。",
        issue.date.format("%Y-%m-%d"),
        normalize_qq_markdown(&overview).trim()
    );
    truncate_at_line(&compact, QQ_MARKDOWN_MAX_CHARS)
}

fn normalize_qq_markdown(source: &str) -> String {
    let mut output = Vec::new();
    let mut previous_blank = true;
    for raw_line in source.lines() {
        let mut line = raw_line.trim_end().to_string();
        if line.trim().is_empty() {
            if !previous_blank {
                output.push(String::new());
            }
            previous_blank = true;
            continue;
        }

        line = HTML_RE.replace_all(&line, "").to_string();
        line = CODE_RE.replace_all(&line, "$1").to_string();
        if line.trim().is_empty() {
            continue;
        }
        if let Some(heading) = line.strip_prefix("### ") {
            line = format!("**{}**", heading.trim());
        } else if line.trim() == "---" {
            line = "***".to_string();
        }

        line = IMAGE_RE
            .replace_all(&line, |captures: &Captures<'_>| {
                let alt = captures.get(1).map_or("", |value| value.as_str()).trim();
                let url = captures.get(2).map_or("", |value| value.as_str());
                if is_public_https_url(url) {
                    format!(
                        "![{}]({url})",
                        if alt.is_empty() { "早报配图" } else { alt }
                    )
                } else {
                    String::new()
                }
            })
            .to_string();
        line = LINK_RE
            .replace_all(&line, |captures: &Captures<'_>| {
                let label = captures.get(1).map_or("", |value| value.as_str());
                let url = captures.get(2).map_or("", |value| value.as_str());
                if is_safe_http_url(url) {
                    format!("[{label}]({url})")
                } else {
                    label.to_string()
                }
            })
            .to_string();

        if (line.starts_with("- ") || line.starts_with("1. ")) && !previous_blank {
            output.push(String::new());
        }
        output.push(line);
        previous_blank = false;
    }
    output.join("\n").trim().to_string()
}

fn extract_markdown_overview(markdown: &str) -> Option<String> {
    let mut output = Vec::new();
    let mut in_overview = false;
    for line in markdown.lines() {
        if line.trim() == "## 概览" {
            in_overview = true;
            output.push(line.to_string());
            continue;
        }
        if in_overview && matches!(line.trim(), "---" | "***") {
            break;
        }
        if in_overview {
            output.push(line.to_string());
        }
    }
    (!output.is_empty()).then(|| output.join("\n"))
}

fn first_public_image(markdown: &str) -> Option<Url> {
    IMAGE_RE.captures_iter(markdown).find_map(|captures| {
        let value = captures.get(2)?.as_str();
        is_public_https_url(value)
            .then(|| Url::parse(value).ok())
            .flatten()
    })
}

fn clean_description(value: &str) -> String {
    let without_html = HTML_RE.replace_all(value, " ");
    let decoded = html_escape::decode_html_entities(&without_html);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clean_inline(value: &str) -> String {
    value
        .replace(['*', '_', '~', '`'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn full_page_url(issue: &Issue) -> &str {
    if issue.page_url.trim().is_empty() {
        &issue.markdown_url
    } else {
        &issue.page_url
    }
}

fn is_http_url(value: &str) -> bool {
    is_safe_http_url(value)
}

fn is_safe_http_url(value: &str) -> bool {
    Url::parse(value).ok().is_some_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
    })
}

pub fn is_public_https_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return false;
    }
    match url.host() {
        Some(url::Host::Ipv4(ip)) => {
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_unspecified())
        }
        Some(url::Host::Ipv6(ip)) => {
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local())
        }
        Some(url::Host::Domain(_)) => true,
        None => false,
    }
}

fn truncate_at_line(value: &str, max_chars: usize) -> String {
    if char_count(value) <= max_chars {
        return value.to_string();
    }
    let mut output = String::new();
    for line in value.lines() {
        let extra = char_count(line) + usize::from(!output.is_empty());
        if char_count(&output) + extra > max_chars {
            break;
        }
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(line);
    }
    output
}

fn char_count(value: &str) -> usize {
    value.chars().count()
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use pretty_assertions::assert_eq;

    use super::*;

    fn issue() -> Issue {
        Issue {
            id: "id".to_string(),
            date: NaiveDate::from_ymd_opt(2026, 8, 10).unwrap(),
            title: "2026-08-10".to_string(),
            page_url: "https://daily.juya.uk/issues/2026-08-10/".to_string(),
            markdown_url: "https://daily.juya.uk/markdown/2026-08-10.md".to_string(),
            description: "fallback".to_string(),
            published_timestamp: 0,
        }
    }

    const MARKDOWN: &str = r#"![](https://assets.juya.uk/cover.png)

# AI 早报 2026-08-10

## 概览
### 开发生态
- OpenRouter 大幅折扣 [↗](https://openrouter.ai/model) `#1`
### 行业动态
- 数据中心投产 [↗](https://example.com/news) `#2`

---

## 开发生态
### [OpenRouter 大幅折扣](https://openrouter.ai/model) `#1`
> 摘要
"#;

    #[test]
    fn renders_onebot_overview_without_markdown_noise() {
        let content = prepare(&issue(), Some(MARKDOWN));
        assert_eq!(
            content.onebot_text,
            "【AI 早报 2026-08-10】\n\n开发生态\n- OpenRouter 大幅折扣\n  https://openrouter.ai/model\n\n行业动态\n- 数据中心投产\n  https://example.com/news\n\n查看全文：https://daily.juya.uk/issues/2026-08-10/\n内容由 AI 辅助创作，请注意核实。"
        );
    }

    #[test]
    fn normalizes_qq_markdown_and_keeps_public_images() {
        let content = prepare(&issue(), Some(MARKDOWN));
        assert!(
            content
                .qq_markdown
                .contains("![早报配图](https://assets.juya.uk/cover.png)")
        );
        assert!(content.qq_markdown.contains("**开发生态**"));
        assert!(content.qq_markdown.contains("***"));
        assert!(!content.qq_markdown.contains("###"));
        assert!(!content.qq_markdown.contains('`'));
    }

    #[test]
    fn extracts_only_public_https_cover() {
        assert!(first_public_image(MARKDOWN).is_some());
        assert!(!is_public_https_url("http://assets.juya.uk/a.png"));
        assert!(!is_public_https_url("https://127.0.0.1/a.png"));
        assert!(!is_public_https_url("https://localhost/a.png"));
    }

    #[test]
    fn removes_unsafe_markdown_links() {
        let normalized = normalize_qq_markdown(
            "[safe](https://example.com) [bad](javascript:alert(1)) [secret](https://u:p@example.com)",
        );
        assert!(normalized.contains("[safe](https://example.com)"));
        assert!(!normalized.contains("javascript:"));
        assert!(!normalized.contains("u:p@"));
    }
}
