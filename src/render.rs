use std::sync::LazyLock;

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use regex::Regex;
use url::Url;

use crate::feed::Issue;

const ONEBOT_MAX_CHARS: usize = 3500;
const QQ_MARKDOWN_MAX_CHARS: usize = 12_000;

static HTML_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<[^>]+>").expect("valid html regex"));
static BLOCKED_HTML_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)<(?:script|style|iframe)\b[^>]*>.*?</(?:script|style|iframe)\s*>")
        .expect("valid blocked html regex")
});
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
    #[derive(Default)]
    struct PlainItem {
        text: String,
        urls: Vec<String>,
    }

    let mut in_overview = false;
    let mut output = Vec::new();
    let mut heading: Option<(HeadingLevel, String)> = None;
    let mut item: Option<PlainItem> = None;
    let mut link_stack = Vec::<Option<String>>::new();
    let mut image_depth = 0_usize;

    for event in markdown_parser(markdown) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some((level, String::new()));
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, text)) = heading.take() {
                    let text = clean_inline(&text);
                    if level == HeadingLevel::H2 && text == "概览" {
                        in_overview = true;
                    } else if in_overview && level == HeadingLevel::H3 && !text.is_empty() {
                        if !output.is_empty()
                            && output
                                .last()
                                .is_some_and(|value: &String| !value.is_empty())
                        {
                            output.push(String::new());
                        }
                        output.push(text);
                    }
                }
            }
            Event::Start(Tag::Item) if in_overview => {
                item = Some(PlainItem::default());
            }
            Event::End(TagEnd::Item) if in_overview => {
                if let Some(mut item) = item.take() {
                    let visible = ISSUE_NUMBER_RE
                        .replace(&clean_inline(&item.text), "")
                        .trim()
                        .to_string();
                    if !visible.is_empty() {
                        output.push(format!("- {visible}"));
                    }
                    item.urls.sort();
                    item.urls.dedup();
                    output.extend(item.urls.into_iter().map(|url| format!("  {url}")));
                }
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                let url = is_safe_http_url(dest_url.as_ref()).then(|| dest_url.to_string());
                if let (Some(item), Some(url)) = (item.as_mut(), url.as_ref()) {
                    item.urls.push(url.clone());
                }
                link_stack.push(url);
            }
            Event::End(TagEnd::Link) => {
                link_stack.pop();
            }
            Event::Start(Tag::Image { .. }) => {
                image_depth += 1;
            }
            Event::End(TagEnd::Image) => {
                image_depth = image_depth.saturating_sub(1);
            }
            Event::Text(text) | Event::Code(text) if image_depth == 0 => {
                if let Some((_, heading)) = heading.as_mut() {
                    heading.push_str(&text);
                } else if let Some(item) = item.as_mut()
                    && (link_stack.is_empty() || text.trim() != "↗")
                {
                    item.text.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak if image_depth == 0 => {
                if let Some((_, heading)) = heading.as_mut() {
                    heading.push(' ');
                } else if let Some(item) = item.as_mut() {
                    item.text.push(' ');
                }
            }
            Event::Rule if in_overview => break,
            _ => {}
        }
    }

    (!output.is_empty()).then(|| output.join("\n"))
}

fn render_qq_markdown(issue: &Issue, markdown: Option<&str>, fallback: &str) -> String {
    let page_url = full_page_url(issue);
    let normalized = normalize_qq_markdown(markdown.unwrap_or(fallback));
    let result = match markdown {
        None => {
            let body = if normalized.trim().is_empty() {
                "今日早报已发布。"
            } else {
                normalized.trim()
            };
            format!(
                "# AI 早报 {}\n\n{body}\n\n[查看网页全文]({page_url})\n\n**提示**：内容由 AI 辅助创作，可能存在幻觉和错误。",
                issue.date.format("%Y-%m-%d")
            )
        }
        Some(_) if normalized.trim().is_empty() => format!(
            "# AI 早报 {}\n\n今日早报已发布。\n\n[查看网页全文]({page_url})\n\n**提示**：内容由 AI 辅助创作，可能存在幻觉和错误。",
            issue.date.format("%Y-%m-%d")
        ),
        Some(_) => normalized,
    };

    if char_count(&result) <= QQ_MARKDOWN_MAX_CHARS {
        return result;
    }

    let overview = markdown
        .and_then(extract_markdown_overview)
        .unwrap_or_else(|| truncate_at_line(fallback, 8000));
    let header = format!("# AI 早报 {}\n\n", issue.date.format("%Y-%m-%d"));
    let footer = format!(
        "\n\n[查看网页全文]({page_url})\n\n**提示**：内容由 AI 辅助创作，可能存在幻觉和错误。"
    );
    let available = QQ_MARKDOWN_MAX_CHARS.saturating_sub(char_count(&header) + char_count(&footer));
    let overview = truncate_at_line(normalize_qq_markdown(&overview).trim(), available);
    format!("{header}{overview}{footer}")
}

fn normalize_qq_markdown(source: &str) -> String {
    let mut writer = QqMarkdownWriter::default();
    let source = BLOCKED_HTML_RE.replace_all(source, "");
    for event in markdown_parser(&source) {
        writer.event(event);
    }
    writer.finish()
}

fn extract_markdown_overview(markdown: &str) -> Option<String> {
    let mut heading: Option<(HeadingLevel, usize, String)> = None;
    let mut overview_start = None;

    for (event, range) in markdown_parser(markdown).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some((level, range.start, String::new()));
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some((_, _, heading)) = heading.as_mut() {
                    heading.push_str(&text);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, start, text)) = heading.take()
                    && level == HeadingLevel::H2
                    && clean_inline(&text) == "概览"
                {
                    overview_start = Some(start);
                }
            }
            Event::Rule => {
                if let Some(start) = overview_start {
                    return markdown
                        .get(start..range.start)
                        .map(str::trim)
                        .map(str::to_string);
                }
            }
            _ => {}
        }
    }

    overview_start.and_then(|start| markdown.get(start..).map(str::trim).map(str::to_string))
}

fn first_public_image(markdown: &str) -> Option<Url> {
    markdown_parser(markdown).find_map(|event| match event {
        Event::Start(Tag::Image { dest_url, .. }) if is_public_https_url(dest_url.as_ref()) => {
            Url::parse(dest_url.as_ref()).ok()
        }
        _ => None,
    })
}

fn markdown_parser(source: &str) -> Parser<'_> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    Parser::new_ext(source, options)
}

#[derive(Default)]
struct QqMarkdownWriter {
    output: String,
    list_stack: Vec<ListState>,
    link_stack: Vec<Option<String>>,
    image_stack: Vec<ImageState>,
    quote_depth: usize,
    item_depth: usize,
    code_block_depth: usize,
}

struct ListState {
    next: Option<u64>,
}

struct ImageState {
    url: Option<String>,
    alt: String,
}

impl QqMarkdownWriter {
    fn event(&mut self, event: Event<'_>) {
        if let Some(image) = self.image_stack.last_mut() {
            match event {
                Event::Text(text) | Event::Code(text) => image.alt.push_str(&text),
                Event::End(TagEnd::Image) => {
                    let image = self.image_stack.pop().expect("image stack");
                    if let Some(url) = image.url {
                        let alt = clean_inline(&image.alt);
                        self.write(&format!(
                            "![{}]({url})",
                            if alt.is_empty() { "早报配图" } else { &alt }
                        ));
                    }
                }
                Event::Start(Tag::Image { dest_url, .. }) => {
                    self.image_stack.push(ImageState {
                        url: is_public_https_url(dest_url.as_ref()).then(|| dest_url.to_string()),
                        alt: String::new(),
                    });
                }
                _ => {}
            }
            return;
        }

        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) | Event::Code(text) => self.write(&sanitize_visible_text(&text)),
            Event::Html(_) | Event::InlineHtml(_) => {}
            Event::SoftBreak | Event::HardBreak => self.write("\n"),
            Event::Rule => {
                self.ensure_blank_line();
                self.write("***");
                self.ensure_blank_line();
            }
            Event::FootnoteReference(name) => self.write(&sanitize_visible_text(&name)),
            Event::TaskListMarker(_) => {}
            _ => {}
        }
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph if self.item_depth == 0 => {
                self.ensure_blank_line();
            }
            Tag::Heading { level, .. } => {
                self.ensure_blank_line();
                self.write(match level {
                    HeadingLevel::H1 => "# ",
                    HeadingLevel::H2 => "## ",
                    _ => "**",
                });
            }
            Tag::BlockQuote(_) => {
                self.ensure_blank_line();
                self.quote_depth += 1;
            }
            Tag::CodeBlock(_) => {
                self.ensure_blank_line();
                self.code_block_depth += 1;
            }
            Tag::List(start) => {
                self.ensure_blank_line();
                self.list_stack.push(ListState { next: start });
            }
            Tag::Item => {
                self.ensure_line_start();
                let indent = "  ".repeat(self.list_stack.len().saturating_sub(1));
                self.write(&indent);
                let prefix = self
                    .list_stack
                    .last_mut()
                    .and_then(|list| list.next.as_mut())
                    .map(|next| {
                        let prefix = format!("{next}. ");
                        *next += 1;
                        prefix
                    })
                    .unwrap_or_else(|| "- ".to_string());
                self.write(&prefix);
                self.item_depth += 1;
            }
            Tag::Emphasis => self.write("_"),
            Tag::Strong => self.write("**"),
            Tag::Strikethrough => self.write("~~"),
            Tag::Link { dest_url, .. } => {
                let url = is_safe_http_url(dest_url.as_ref()).then(|| dest_url.to_string());
                if url.is_some() {
                    self.write("[");
                }
                self.link_stack.push(url);
            }
            Tag::Image { dest_url, .. } => {
                self.image_stack.push(ImageState {
                    url: is_public_https_url(dest_url.as_ref()).then(|| dest_url.to_string()),
                    alt: String::new(),
                });
            }
            _ => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph if self.item_depth == 0 => {
                self.ensure_blank_line();
            }
            TagEnd::Heading(level) => {
                if !matches!(level, HeadingLevel::H1 | HeadingLevel::H2) {
                    self.write("**");
                }
                self.ensure_blank_line();
            }
            TagEnd::BlockQuote(_) => {
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.ensure_blank_line();
            }
            TagEnd::CodeBlock => {
                self.code_block_depth = self.code_block_depth.saturating_sub(1);
                self.ensure_blank_line();
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.ensure_blank_line();
            }
            TagEnd::Item => {
                self.item_depth = self.item_depth.saturating_sub(1);
                self.ensure_line_start();
            }
            TagEnd::Emphasis => self.write("_"),
            TagEnd::Strong => self.write("**"),
            TagEnd::Strikethrough => self.write("~~"),
            TagEnd::Link => {
                if let Some(Some(url)) = self.link_stack.pop() {
                    self.write(&format!("]({url})"));
                }
            }
            TagEnd::Image => {}
            _ => {}
        }
    }

    fn write(&mut self, value: &str) {
        for character in value.chars() {
            if self.at_line_start() && self.quote_depth > 0 && character != '\n' {
                self.output.push_str(&"> ".repeat(self.quote_depth));
            }
            self.output.push(character);
        }
    }

    fn ensure_line_start(&mut self) {
        if !self.output.is_empty() && !self.output.ends_with('\n') {
            self.output.push('\n');
        }
    }

    fn ensure_blank_line(&mut self) {
        while self.output.ends_with([' ', '\t']) {
            self.output.pop();
        }
        if self.output.is_empty() {
            return;
        }
        let newlines = self
            .output
            .chars()
            .rev()
            .take_while(|character| *character == '\n')
            .count();
        for _ in newlines..2 {
            self.output.push('\n');
        }
    }

    fn at_line_start(&self) -> bool {
        self.output.is_empty() || self.output.ends_with('\n')
    }

    fn finish(self) -> String {
        normalize_rendered_lines(&self.output)
    }
}

fn normalize_rendered_lines(value: &str) -> String {
    let mut lines = Vec::new();
    let mut previous_blank = true;
    for line in value.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            if !previous_blank {
                lines.push(String::new());
            }
            previous_blank = true;
        } else {
            lines.push(line.to_string());
            previous_blank = false;
        }
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

fn sanitize_visible_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| {
            (!character.is_control() || matches!(character, '\n' | '\t'))
                && !is_direction_control(*character)
        })
        .collect()
}

fn clean_description(value: &str) -> String {
    let without_html = HTML_TAG_RE.replace_all(value, " ");
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

fn is_safe_http_url(value: &str) -> bool {
    if value.chars().any(is_disallowed_url_char) {
        return false;
    }
    Url::parse(value).ok().is_some_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
    })
}

pub fn is_public_https_url(value: &str) -> bool {
    if value.chars().any(is_disallowed_url_char) {
        return false;
    }
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
        Some(url::Host::Ipv4(ip)) => !is_blocked_ipv4(ip),
        Some(url::Host::Ipv6(ip)) => {
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_multicast()
                || matches!(ip.segments(), [0x2001, 0x0db8, ..]))
        }
        Some(url::Host::Domain(_)) => true,
        None => false,
    }
}

fn is_blocked_ipv4(ip: std::net::Ipv4Addr) -> bool {
    let octets = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_documentation()
        || octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 198 && matches!(octets[1], 18 | 19))
        || octets[0] >= 240
}

fn is_disallowed_url_char(value: char) -> bool {
    value.is_control() || is_direction_control(value)
}

fn is_direction_control(value: char) -> bool {
    matches!(
        value,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
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
    fn fixture_snapshots_match_onebot_and_official_outputs() {
        let source = include_str!("../fixtures/markdown/juya-structure.md");
        let content = prepare(&issue(), Some(source));
        let expected_onebot = include_str!("../fixtures/expected/onebot.txt")
            .replace("\r\n", "\n")
            .trim_end()
            .to_string();
        let expected_official = include_str!("../fixtures/expected/qq-official.md")
            .replace("\r\n", "\n")
            .trim_end()
            .to_string();

        assert_eq!(content.onebot_text, expected_onebot);
        assert_eq!(content.qq_markdown, expected_official);
    }

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
        assert!(!is_public_https_url("https://224.0.0.1/a.png"));
        assert!(!is_public_https_url("https://240.0.0.1/a.png"));
        assert!(!is_public_https_url("https://192.0.2.1/a.png"));
        assert!(!is_public_https_url("https://[ff02::1]/a.png"));
        assert!(!is_public_https_url("https://[2001:db8::1]/a.png"));
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

    #[test]
    fn keeps_only_public_https_markdown_images() {
        let normalized = normalize_qq_markdown(
            "![http](http://example.com/a.png)\n![private](https://127.0.0.1/a.png)\n![public](https://example.com/a.png)",
        );

        assert!(!normalized.contains("http://example.com/a.png"));
        assert!(!normalized.contains("127.0.0.1"));
        assert!(normalized.contains("![public](https://example.com/a.png)"));
    }

    #[test]
    fn qq_fallback_keeps_title_link_and_disclaimer() {
        let content = prepare(&issue(), None);

        assert!(content.qq_markdown.starts_with("# AI 早报 2026-08-10"));
        assert!(content.qq_markdown.contains("fallback"));
        assert!(content.qq_markdown.contains("[查看网页全文]"));
        assert!(content.qq_markdown.contains("AI 辅助创作"));
    }

    #[test]
    fn empty_content_uses_stable_fallbacks() {
        let mut empty_issue = issue();
        empty_issue.description.clear();
        let content = prepare(&empty_issue, None);

        assert!(content.onebot_text.contains("今日早报已发布。"));
        assert!(content.qq_markdown.contains("今日早报已发布。"));
        assert!(content.onebot_text.contains("查看全文"));
        assert!(content.qq_markdown.contains("查看网页全文"));
    }

    #[test]
    fn long_outputs_keep_required_footers_within_limits() {
        let items = (0..1000)
            .map(|index| format!("- 新闻 {index} [↗](https://example.com/{index}) `#{index}`"))
            .collect::<Vec<_>>()
            .join("\n");
        let markdown = format!(
            "# AI 早报\n\n## 概览\n### 分类\n{items}\n\n---\n\n{}",
            "正文\n".repeat(13_000)
        );
        let content = prepare(&issue(), Some(&markdown));

        assert!(char_count(&content.onebot_text) <= ONEBOT_MAX_CHARS);
        assert!(content.onebot_text.contains("查看全文"));
        assert!(
            content
                .onebot_text
                .ends_with("内容由 AI 辅助创作，请注意核实。")
        );
        assert!(char_count(&content.qq_markdown) <= QQ_MARKDOWN_MAX_CHARS);
        assert!(content.qq_markdown.contains("查看网页全文"));
        assert!(content.qq_markdown.contains("AI 辅助创作"));
    }

    #[test]
    fn removes_blocked_html_code_fences_and_direction_controls() {
        let source = "<script>alert('secret')</script>\n<style>.x{}</style>\n<iframe>hidden</iframe>\n```rust\nlet x = 1;\n```\n[safe](https://example.com)\n[hidden](https://exa\u{202e}mple.com)";
        let normalized = normalize_qq_markdown(source);

        assert!(!normalized.contains("alert"));
        assert!(!normalized.contains(".x"));
        assert!(!normalized.contains("hidden</"));
        assert!(!normalized.contains("```"));
        assert!(normalized.contains("let x = 1;"));
        assert!(normalized.contains("[safe](https://example.com)"));
        assert!(!normalized.contains('\u{202e}'));
    }
}
