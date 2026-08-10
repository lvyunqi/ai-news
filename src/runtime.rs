use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, RwLock};
use std::thread::{self, JoinHandle};

use abi_stable_host_api::SendEnqueueStatus;
use chrono::{DateTime, Utc};
use url::Url;

use crate::config::{ImageMode, Protocol, RuntimeConfig};
use crate::delivery::{DeliverySender, HostDeliverySender, status_name};
use crate::feed::{ContentSource, HttpContentSource, parse_latest_today};
use crate::media::download_cover;
use crate::render::prepare;
use crate::state::DeliveryState;

static STOP: AtomicBool = AtomicBool::new(false);
static WORKER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
static STATUS: LazyLock<RwLock<StatusSnapshot>> =
    LazyLock::new(|| RwLock::new(StatusSnapshot::default()));

#[derive(Clone, Debug)]
pub struct StatusSnapshot {
    pub plugin_enabled: bool,
    pub worker_state: String,
    pub configured_targets: usize,
    pub enabled_targets: usize,
    pub last_poll_at: Option<String>,
    pub last_result: String,
    pub last_issue_date: Option<String>,
    pub target_results: Vec<TargetResult>,
}

impl Default for StatusSnapshot {
    fn default() -> Self {
        Self {
            plugin_enabled: false,
            worker_state: "stopped".to_string(),
            configured_targets: 0,
            enabled_targets: 0,
            last_poll_at: None,
            last_result: "尚未轮询".to_string(),
            last_issue_date: None,
            target_results: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TargetResult {
    pub name: String,
    pub protocol: String,
    pub account_id: String,
    pub group_id: String,
    pub status: String,
}

#[derive(Clone, Debug, Default)]
struct PollResult {
    issue_date: Option<String>,
    accepted: usize,
    skipped: usize,
    failed: usize,
    targets: Vec<TargetResult>,
    message: String,
}

pub fn start(config: RuntimeConfig, data_dir: PathBuf) -> Result<(), String> {
    stop()?;
    let configured_targets = config.targets.len();
    let enabled_targets = config
        .targets
        .iter()
        .filter(|target| target.enabled)
        .count();
    replace_status(StatusSnapshot {
        plugin_enabled: config.enabled,
        worker_state: "stopped".to_string(),
        configured_targets,
        enabled_targets,
        last_poll_at: None,
        last_result: if config.enabled {
            "等待启动".to_string()
        } else {
            "插件配置为关闭".to_string()
        },
        last_issue_date: None,
        target_results: Vec::new(),
    });

    if !config.enabled || enabled_targets == 0 {
        if config.enabled && enabled_targets == 0 {
            update_status(|status| status.last_result = "没有启用的推送目标".to_string());
        }
        return Ok(());
    }

    let source = HttpContentSource::new(config.request_timeout)?;
    let state = match DeliveryState::load(&data_dir) {
        Ok(state) => state,
        Err(error) => {
            update_status(|status| {
                status.worker_state = "protected".to_string();
                status.last_result = error.clone();
            });
            eprintln!("[ai-news][state_load] {error}");
            return Ok(());
        }
    };

    STOP.store(false, Ordering::Release);
    let mut slot = WORKER.lock().map_err(|_| "后台线程锁已损坏".to_string())?;
    let handle = thread::Builder::new()
        .name("qimen-ai-news".to_string())
        .spawn(move || worker_loop(config, data_dir, source, state, HostDeliverySender))
        .map_err(|error| format!("启动后台线程失败：{error}"))?;
    *slot = Some(handle);
    update_status(|status| status.worker_state = "running".to_string());
    Ok(())
}

pub fn stop() -> Result<(), String> {
    STOP.store(true, Ordering::Release);
    let handle = WORKER
        .lock()
        .map_err(|_| "后台线程锁已损坏".to_string())?
        .take();
    if let Some(handle) = handle {
        handle.thread().unpark();
        handle
            .join()
            .map_err(|_| "后台线程停止时发生 panic".to_string())?;
    }
    update_status(|status| status.worker_state = "stopped".to_string());
    Ok(())
}

fn worker_loop(
    config: RuntimeConfig,
    data_dir: PathBuf,
    source: impl ContentSource,
    mut state: DeliveryState,
    sender: impl DeliverySender,
) {
    while !STOP.load(Ordering::Acquire) {
        let now = Utc::now();
        let result = run_poll(&config, &data_dir, &source, &sender, &mut state, now);
        match result {
            Ok(result) => apply_poll_result(result, now),
            Err(error) => {
                eprintln!("[ai-news][poll] {error}");
                update_status(|status| {
                    status.last_poll_at = Some(now.to_rfc3339());
                    status.last_result = error.clone();
                });
                if error.starts_with("状态写入失败") {
                    update_status(|status| status.worker_state = "protected".to_string());
                    break;
                }
            }
        }
        if STOP.load(Ordering::Acquire) {
            break;
        }
        thread::park_timeout(config.poll_interval);
    }
    update_status(|status| {
        if status.worker_state != "protected" {
            status.worker_state = "stopped".to_string();
        }
    });
}

fn run_poll(
    config: &RuntimeConfig,
    data_dir: &Path,
    source: &dyn ContentSource,
    sender: &dyn DeliverySender,
    state: &mut DeliveryState,
    now: DateTime<Utc>,
) -> Result<PollResult, String> {
    let rss = source
        .fetch_rss(&config.feed_url)
        .map_err(|error| format!("RSS 获取失败：{error}"))?;
    let today = now.with_timezone(&config.timezone).date_naive();
    let Some(issue) = parse_latest_today(&rss, today, config.timezone, &config.feed_url)? else {
        return Ok(PollResult {
            message: format!("{} 没有新的当天早报", today.format("%Y-%m-%d")),
            ..PollResult::default()
        });
    };

    let pending = config
        .targets
        .iter()
        .filter(|target| target.enabled && !state.contains(&target.key(), &issue.id))
        .count();
    if pending == 0 {
        return Ok(PollResult {
            issue_date: Some(issue.date.to_string()),
            skipped: config
                .targets
                .iter()
                .filter(|target| target.enabled)
                .count(),
            message: "本期已对所有启用目标入队".to_string(),
            ..PollResult::default()
        });
    }

    let markdown =
        Url::parse(&issue.markdown_url)
            .ok()
            .and_then(|url| match source.fetch_markdown(&url) {
                Ok(value) => Some(value),
                Err(error) => {
                    eprintln!("[ai-news][content_fetch] Markdown 获取失败，使用 RSS 降级：{error}");
                    None
                }
            });
    let content = prepare(&issue, markdown.as_deref());
    let mut cover_cache: Option<Result<String, String>> = None;
    let mut result = PollResult {
        issue_date: Some(issue.date.to_string()),
        message: String::new(),
        ..PollResult::default()
    };

    for target in config.targets.iter().filter(|target| target.enabled) {
        if STOP.load(Ordering::Acquire) {
            break;
        }
        let key = target.key();
        if state.contains(&key, &issue.id) {
            result.skipped += 1;
            result.targets.push(target_result(target, "Skipped"));
            continue;
        }

        let cover =
            if target.protocol == Protocol::OneBot11 && target.image_mode == ImageMode::Cover {
                if cover_cache.is_none() {
                    cover_cache = Some(match content.cover_url.as_ref() {
                        Some(url) => download_cover(source, url),
                        None => Err("早报没有可用封面".to_string()),
                    });
                }
                match cover_cache.as_ref().expect("cover cache initialized") {
                    Ok(value) => Some(value.as_str()),
                    Err(error) => {
                        eprintln!(
                            "[ai-news][cover_fetch] target={} 封面不可用，继续发送文本：{}",
                            target.name, error
                        );
                        None
                    }
                }
            } else {
                None
            };

        let status = sender.send(target, &content, cover);
        let status_text = status_name(status);
        result.targets.push(target_result(target, status_text));
        eprintln!(
            "[ai-news][enqueue] target={} protocol={} account={} group={} status={}",
            target.name,
            target.protocol.as_str(),
            mask(&target.account_id),
            mask(&target.group_id),
            status_text
        );

        if status == SendEnqueueStatus::Accepted {
            state
                .mark_accepted(data_dir, key, issue.id.clone(), issue.date.to_string())
                .map_err(|error| format!("状态写入失败：{error}"))?;
            result.accepted += 1;
        } else {
            result.failed += 1;
            if status == SendEnqueueStatus::HostShuttingDown {
                STOP.store(true, Ordering::Release);
                break;
            }
        }
    }

    result.message = format!(
        "期号 {}：已入队 {}，跳过 {}，失败 {}",
        issue.date, result.accepted, result.skipped, result.failed
    );
    Ok(result)
}

fn apply_poll_result(result: PollResult, now: DateTime<Utc>) {
    eprintln!("[ai-news][poll] {}", result.message);
    update_status(|status| {
        status.last_poll_at = Some(now.to_rfc3339());
        status.last_result = result.message;
        status.last_issue_date = result.issue_date;
        status.target_results = result.targets;
    });
}

fn target_result(target: &crate::config::Target, status: &str) -> TargetResult {
    TargetResult {
        name: target.name.clone(),
        protocol: target.protocol.as_str().to_string(),
        account_id: mask(&target.account_id),
        group_id: mask(&target.group_id),
        status: status.to_string(),
    }
}

pub fn status_text() -> String {
    let snapshot = STATUS
        .read()
        .map(|status| status.clone())
        .unwrap_or_else(|_| StatusSnapshot {
            last_result: "状态锁已损坏".to_string(),
            ..StatusSnapshot::default()
        });
    let mut lines = vec![
        format!("AI 早报插件 v{}", env!("CARGO_PKG_VERSION")),
        format!(
            "配置：{}，Worker：{}",
            if snapshot.plugin_enabled {
                "启用"
            } else {
                "关闭"
            },
            snapshot.worker_state
        ),
        format!(
            "目标：启用 {}/配置 {}",
            snapshot.enabled_targets, snapshot.configured_targets
        ),
        format!(
            "最近轮询：{}",
            snapshot.last_poll_at.as_deref().unwrap_or("尚未轮询")
        ),
        format!("最近结果：{}", snapshot.last_result),
    ];
    if let Some(date) = snapshot.last_issue_date {
        lines.push(format!("最近期号：{date}"));
    }
    for target in snapshot.target_results.iter().take(8) {
        lines.push(format!(
            "- {} [{}] {}/{}：{}",
            target.name, target.protocol, target.account_id, target.group_id, target.status
        ));
    }
    if snapshot.target_results.len() > 8 {
        lines.push(format!(
            "- 其余 {} 个目标已省略",
            snapshot.target_results.len() - 8
        ));
    }
    lines.join("\n")
}

fn replace_status(status: StatusSnapshot) {
    if let Ok(mut slot) = STATUS.write() {
        *slot = status;
    }
}

fn update_status(update: impl FnOnce(&mut StatusSnapshot)) {
    if let Ok(mut status) = STATUS.write() {
        update(&mut status);
    }
}

fn mask(value: &str) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    if chars.len() <= 6 {
        return "***".to_string();
    }
    format!(
        "{}***{}",
        chars[..3].iter().collect::<String>(),
        chars[chars.len() - 3..].iter().collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use abi_stable_host_api::SendEnqueueStatus;
    use chrono::TimeZone;

    use super::*;
    use crate::config::{ImageMode, Target};
    use crate::render::PreparedContent;

    const RSS: &str = r#"<rss version="2.0"><channel><title>AI</title><item>
      <title>2026-08-10</title><link>https://daily.juya.uk/issues/2026-08-10/</link>
      <guid>issue-1</guid><pubDate>Mon, 10 Aug 2026 01:30:00 GMT</pubDate>
      <description>fallback</description></item></channel></rss>"#;
    const MARKDOWN: &str = r#"![](https://assets.juya.uk/cover.png)
# AI 早报 2026-08-10
## 概览
### 开发生态
- 新闻 [↗](https://example.com/news) `#1`
---
## 正文
内容"#;

    struct FakeSource;

    impl ContentSource for FakeSource {
        fn fetch_rss(&self, _url: &Url) -> Result<Vec<u8>, String> {
            Ok(RSS.as_bytes().to_vec())
        }

        fn fetch_markdown(&self, _url: &Url) -> Result<String, String> {
            Ok(MARKDOWN.to_string())
        }

        fn fetch_image(&self, _url: &Url) -> Result<Vec<u8>, String> {
            Ok(vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a])
        }
    }

    struct FakeSender {
        calls: StdMutex<Vec<String>>,
    }

    impl DeliverySender for FakeSender {
        fn send(
            &self,
            target: &Target,
            _content: &PreparedContent,
            _cover_base64: Option<&str>,
        ) -> SendEnqueueStatus {
            self.calls.lock().unwrap().push(target.key());
            SendEnqueueStatus::Accepted
        }
    }

    fn config() -> RuntimeConfig {
        RuntimeConfig {
            enabled: true,
            feed_url: Url::parse("https://daily.juya.uk/rss.xml").unwrap(),
            timezone: chrono_tz::Asia::Shanghai,
            poll_interval: std::time::Duration::from_secs(60),
            request_timeout: std::time::Duration::from_secs(15),
            targets: vec![Target {
                name: "group".to_string(),
                enabled: true,
                protocol: Protocol::OneBot11,
                account_id: "bot".to_string(),
                group_id: "group".to_string(),
                image_mode: ImageMode::None,
            }],
        }
    }

    fn mixed_config() -> RuntimeConfig {
        let mut config = config();
        config.targets.push(Target {
            name: "official".to_string(),
            enabled: true,
            protocol: Protocol::QqOfficial,
            account_id: "app".to_string(),
            group_id: "group-openid".to_string(),
            image_mode: ImageMode::None,
        });
        config
    }

    #[test]
    fn poll_sends_once_and_persists_deduplication() {
        STOP.store(false, Ordering::Release);
        let dir = tempfile::tempdir().unwrap();
        let source = FakeSource;
        let sender = FakeSender {
            calls: StdMutex::new(Vec::new()),
        };
        let mut state = DeliveryState::load(dir.path()).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 10, 2, 0, 0).unwrap();

        let first = run_poll(&config(), dir.path(), &source, &sender, &mut state, now).unwrap();
        let second = run_poll(&config(), dir.path(), &source, &sender, &mut state, now).unwrap();

        assert_eq!(first.accepted, 1);
        assert_eq!(second.skipped, 1);
        assert_eq!(sender.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn masks_ids_in_status_output() {
        assert_eq!(mask("123456789"), "123***789");
        assert_eq!(mask("short"), "***");
    }

    #[test]
    fn poll_fans_out_to_mixed_protocol_targets() {
        STOP.store(false, Ordering::Release);
        let dir = tempfile::tempdir().unwrap();
        let source = FakeSource;
        let sender = FakeSender {
            calls: StdMutex::new(Vec::new()),
        };
        let mut state = DeliveryState::load(dir.path()).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 10, 2, 0, 0).unwrap();

        let result = run_poll(
            &mixed_config(),
            dir.path(),
            &source,
            &sender,
            &mut state,
            now,
        )
        .unwrap();

        assert_eq!(result.accepted, 2);
        assert_eq!(sender.calls.lock().unwrap().len(), 2);
        assert!(state.contains("onebot11|bot|group", "issue-1"));
        assert!(state.contains("qq-official|app|group-openid", "issue-1"));
    }
}
