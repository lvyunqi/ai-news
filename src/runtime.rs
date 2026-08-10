use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

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
    pub last_poll_duration_ms: Option<u128>,
    pub last_result: String,
    pub last_issue_date: Option<String>,
    pub last_issue_hash: Option<String>,
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
            last_poll_duration_ms: None,
            last_result: "尚未轮询".to_string(),
            last_issue_date: None,
            last_issue_hash: None,
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
    issue_id: Option<String>,
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
        last_poll_duration_ms: None,
        last_result: if config.enabled {
            "等待启动".to_string()
        } else {
            "插件配置为关闭".to_string()
        },
        last_issue_date: None,
        last_issue_hash: None,
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
        let started = Instant::now();
        let result = run_poll(&config, &data_dir, &source, &sender, &mut state, now, &STOP);
        let elapsed = started.elapsed();
        match result {
            Ok(result) => apply_poll_result(result, now, elapsed),
            Err(error) => {
                eprintln!("[ai-news][poll] {error}");
                update_status(|status| {
                    status.last_poll_at = Some(now.to_rfc3339());
                    status.last_poll_duration_ms = Some(elapsed.as_millis());
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
    stop: &AtomicBool,
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
            issue_id: Some(issue.id.clone()),
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
        issue_id: Some(issue.id.clone()),
        message: String::new(),
        ..PollResult::default()
    };

    for target in config.targets.iter().filter(|target| target.enabled) {
        if stop.load(Ordering::Acquire) {
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
        result
            .targets
            .push(target_result(target, target_status_text(status)));
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
                stop.store(true, Ordering::Release);
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

fn apply_poll_result(result: PollResult, now: DateTime<Utc>, elapsed: Duration) {
    eprintln!(
        "[ai-news][poll] {} duration_ms={} targets={}",
        result.message,
        elapsed.as_millis(),
        result.targets.len()
    );
    update_status(|status| {
        status.last_poll_at = Some(now.to_rfc3339());
        status.last_poll_duration_ms = Some(elapsed.as_millis());
        status.last_result = result.message;
        status.last_issue_date = result.issue_date;
        status.last_issue_hash = result.issue_id.as_deref().map(issue_hash_prefix);
        status.target_results = result.targets;
    });
}

fn target_status_text(status: SendEnqueueStatus) -> &'static str {
    if status == SendEnqueueStatus::Accepted {
        "宿主已接收入队"
    } else {
        status_name(status)
    }
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
        format!(
            "AI 早报插件 v{}（动态 API 0.6 / 配置 v1）",
            env!("CARGO_PKG_VERSION")
        ),
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
        let hash = snapshot.last_issue_hash.as_deref().unwrap_or("未知");
        lines.push(format!("最近期号：{date}（ID {hash}）"));
    }
    if let Some(duration) = snapshot.last_poll_duration_ms {
        lines.push(format!("轮询耗时：{duration} ms"));
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

fn issue_hash_prefix(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")[..8].to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::AtomicUsize;

    use abi_stable_host_api::SendEnqueueStatus;
    use chrono::TimeZone;

    use super::*;
    use crate::config::{ImageMode, Target};
    use crate::feed::ImageResponse;
    use crate::render::PreparedContent;
    use crate::state::state_path;

    static TEST_LOCK: StdMutex<()> = StdMutex::new(());

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

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

    #[derive(Default)]
    struct FakeSource {
        rss_calls: AtomicUsize,
        markdown_calls: AtomicUsize,
        image_calls: AtomicUsize,
    }

    impl ContentSource for FakeSource {
        fn fetch_rss(&self, _url: &Url) -> Result<Vec<u8>, String> {
            self.rss_calls.fetch_add(1, Ordering::Relaxed);
            Ok(RSS.as_bytes().to_vec())
        }

        fn fetch_markdown(&self, _url: &Url) -> Result<String, String> {
            self.markdown_calls.fetch_add(1, Ordering::Relaxed);
            Ok(MARKDOWN.to_string())
        }

        fn fetch_image(&self, _url: &Url) -> Result<ImageResponse, String> {
            self.image_calls.fetch_add(1, Ordering::Relaxed);
            Ok(ImageResponse {
                bytes: vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0],
                content_type: Some("image/png".to_string()),
            })
        }
    }

    #[derive(Clone, Debug)]
    struct SendCall {
        key: String,
        protocol: Protocol,
        has_cover: bool,
        onebot_text: String,
        qq_markdown: String,
    }

    struct FakeSender {
        calls: StdMutex<Vec<SendCall>>,
        statuses: StdMutex<VecDeque<SendEnqueueStatus>>,
    }

    impl FakeSender {
        fn accepted() -> Self {
            Self::scripted(Vec::new())
        }

        fn scripted(statuses: Vec<SendEnqueueStatus>) -> Self {
            Self {
                calls: StdMutex::new(Vec::new()),
                statuses: StdMutex::new(statuses.into()),
            }
        }
    }

    impl DeliverySender for FakeSender {
        fn send(
            &self,
            target: &Target,
            content: &PreparedContent,
            cover_base64: Option<&str>,
        ) -> SendEnqueueStatus {
            self.calls.lock().unwrap().push(SendCall {
                key: target.key(),
                protocol: target.protocol,
                has_cover: cover_base64.is_some(),
                onebot_text: content.onebot_text.clone(),
                qq_markdown: content.qq_markdown.clone(),
            });
            self.statuses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(SendEnqueueStatus::Accepted)
        }
    }

    fn target(name: &str, protocol: Protocol, account: &str, group: &str) -> Target {
        Target {
            name: name.to_string(),
            enabled: true,
            protocol,
            account_id: account.to_string(),
            group_id: group.to_string(),
            image_mode: ImageMode::None,
        }
    }

    fn config() -> RuntimeConfig {
        RuntimeConfig {
            enabled: true,
            feed_url: Url::parse("https://daily.juya.uk/rss.xml").unwrap(),
            timezone: chrono_tz::Asia::Shanghai,
            poll_interval: std::time::Duration::from_secs(60),
            request_timeout: std::time::Duration::from_secs(15),
            targets: vec![target("group", Protocol::OneBot11, "bot", "group")],
        }
    }

    fn mixed_config() -> RuntimeConfig {
        let mut config = config();
        config.targets.push(target(
            "official",
            Protocol::QqOfficial,
            "app",
            "group-openid",
        ));
        config
    }

    #[test]
    fn poll_sends_once_and_persists_deduplication() {
        let _guard = test_guard();
        let dir = tempfile::tempdir().unwrap();
        let source = FakeSource::default();
        let sender = FakeSender::accepted();
        let mut state = DeliveryState::load(dir.path()).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 10, 2, 0, 0).unwrap();
        let stop = AtomicBool::new(false);

        let first = run_poll(
            &config(),
            dir.path(),
            &source,
            &sender,
            &mut state,
            now,
            &stop,
        )
        .unwrap();
        let second = run_poll(
            &config(),
            dir.path(),
            &source,
            &sender,
            &mut state,
            now,
            &stop,
        )
        .unwrap();

        assert_eq!(first.accepted, 1);
        assert_eq!(second.skipped, 1);
        assert_eq!(sender.calls.lock().unwrap().len(), 1);
        assert_eq!(source.rss_calls.load(Ordering::Relaxed), 2);
        assert_eq!(source.markdown_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn masks_ids_in_status_output() {
        let _guard = test_guard();
        assert_eq!(mask("123456789"), "123***789");
        assert_eq!(mask("short"), "***");
        assert_eq!(mask("机器人账号一二三四"), "机器人***二三四");
    }

    #[test]
    fn poll_fans_out_to_mixed_protocol_targets() {
        let _guard = test_guard();
        let dir = tempfile::tempdir().unwrap();
        let source = FakeSource::default();
        let sender = FakeSender::accepted();
        let mut state = DeliveryState::load(dir.path()).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 10, 2, 0, 0).unwrap();
        let stop = AtomicBool::new(false);

        let result = run_poll(
            &mixed_config(),
            dir.path(),
            &source,
            &sender,
            &mut state,
            now,
            &stop,
        )
        .unwrap();

        assert_eq!(result.accepted, 2);
        let calls = sender.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].protocol, Protocol::OneBot11);
        assert_eq!(calls[1].protocol, Protocol::QqOfficial);
        assert!(calls[0].onebot_text.starts_with("【AI 早报"));
        assert!(calls[1].qq_markdown.starts_with("![早报配图]"));
        assert!(state.contains("onebot11|bot|group", "issue-1"));
        assert!(state.contains("qq-official|app|group-openid", "issue-1"));
        assert_eq!(source.rss_calls.load(Ordering::Relaxed), 1);
        assert_eq!(source.markdown_calls.load(Ordering::Relaxed), 1);
        assert_eq!(source.image_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn non_accepted_targets_are_not_persisted_and_later_targets_continue() {
        let _guard = test_guard();
        let dir = tempfile::tempdir().unwrap();
        let source = FakeSource::default();
        let sender = FakeSender::scripted(vec![
            SendEnqueueStatus::QueueFull,
            SendEnqueueStatus::BotNotFound,
            SendEnqueueStatus::Accepted,
        ]);
        let mut config = config();
        config.targets = vec![
            target("queue", Protocol::OneBot11, "bot-1", "group-1"),
            target("missing", Protocol::OneBot11, "bot-2", "group-2"),
            target("ok", Protocol::QqOfficial, "app", "group-openid"),
        ];
        let mut state = DeliveryState::load(dir.path()).unwrap();
        let stop = AtomicBool::new(false);

        let result = run_poll(
            &config,
            dir.path(),
            &source,
            &sender,
            &mut state,
            Utc.with_ymd_and_hms(2026, 8, 10, 2, 0, 0).unwrap(),
            &stop,
        )
        .unwrap();

        assert_eq!(result.accepted, 1);
        assert_eq!(result.failed, 2);
        assert_eq!(sender.calls.lock().unwrap().len(), 3);
        assert!(!state.contains("onebot11|bot-1|group-1", "issue-1"));
        assert!(!state.contains("onebot11|bot-2|group-2", "issue-1"));
        assert!(state.contains("qq-official|app|group-openid", "issue-1"));
        assert_eq!(result.targets[2].status, "宿主已接收入队");
    }

    #[test]
    fn multiple_cover_targets_download_and_encode_once() {
        let _guard = test_guard();
        let dir = tempfile::tempdir().unwrap();
        let source = FakeSource::default();
        let sender = FakeSender::accepted();
        let mut config = config();
        config.targets = vec![
            target("a", Protocol::OneBot11, "bot-a", "group-a"),
            target("b", Protocol::OneBot11, "bot-b", "group-b"),
        ];
        for target in &mut config.targets {
            target.image_mode = ImageMode::Cover;
        }
        let mut state = DeliveryState::load(dir.path()).unwrap();

        run_poll(
            &config,
            dir.path(),
            &source,
            &sender,
            &mut state,
            Utc.with_ymd_and_hms(2026, 8, 10, 2, 0, 0).unwrap(),
            &AtomicBool::new(false),
        )
        .unwrap();

        assert_eq!(source.image_calls.load(Ordering::Relaxed), 1);
        assert!(
            sender
                .calls
                .lock()
                .unwrap()
                .iter()
                .all(|call| call.has_cover)
        );
    }

    #[test]
    fn host_shutting_down_stops_current_target_loop() {
        let _guard = test_guard();
        let dir = tempfile::tempdir().unwrap();
        let source = FakeSource::default();
        let sender = FakeSender::scripted(vec![SendEnqueueStatus::HostShuttingDown]);
        let mut config = mixed_config();
        config.targets.push(target(
            "later",
            Protocol::OneBot11,
            "bot-later",
            "group-later",
        ));
        let mut state = DeliveryState::load(dir.path()).unwrap();
        let stop = AtomicBool::new(false);

        let result = run_poll(
            &config,
            dir.path(),
            &source,
            &sender,
            &mut state,
            Utc.with_ymd_and_hms(2026, 8, 10, 2, 0, 0).unwrap(),
            &stop,
        )
        .unwrap();

        assert!(stop.load(Ordering::Acquire));
        assert_eq!(result.failed, 1);
        assert_eq!(sender.calls.lock().unwrap().len(), 1);
        assert!(state.deliveries.is_empty());
    }

    #[test]
    fn disabled_target_is_never_sent_or_persisted() {
        let _guard = test_guard();
        let dir = tempfile::tempdir().unwrap();
        let source = FakeSource::default();
        let sender = FakeSender::accepted();
        let mut config = mixed_config();
        config.targets[0].enabled = false;
        let mut state = DeliveryState::load(dir.path()).unwrap();

        let result = run_poll(
            &config,
            dir.path(),
            &source,
            &sender,
            &mut state,
            Utc.with_ymd_and_hms(2026, 8, 10, 2, 0, 0).unwrap(),
            &AtomicBool::new(false),
        )
        .unwrap();

        let calls = sender.calls.lock().unwrap();
        assert_eq!(result.accepted, 1);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].key, "qq-official|app|group-openid");
        assert!(!state.contains("onebot11|bot|group", "issue-1"));
    }

    #[test]
    fn fifty_queue_full_targets_do_not_repeat_http_or_send_attempts() {
        let _guard = test_guard();
        let dir = tempfile::tempdir().unwrap();
        let source = FakeSource::default();
        let sender = FakeSender::scripted(vec![SendEnqueueStatus::QueueFull; 50]);
        let mut config = config();
        config.targets = (0..50)
            .map(|index| {
                target(
                    &format!("target-{index}"),
                    Protocol::OneBot11,
                    &format!("bot-{index}"),
                    &format!("group-{index}"),
                )
            })
            .collect();
        let mut state = DeliveryState::load(dir.path()).unwrap();

        let result = run_poll(
            &config,
            dir.path(),
            &source,
            &sender,
            &mut state,
            Utc.with_ymd_and_hms(2026, 8, 10, 2, 0, 0).unwrap(),
            &AtomicBool::new(false),
        )
        .unwrap();

        assert_eq!(result.failed, 50);
        assert_eq!(sender.calls.lock().unwrap().len(), 50);
        assert_eq!(source.rss_calls.load(Ordering::Relaxed), 1);
        assert_eq!(source.markdown_calls.load(Ordering::Relaxed), 1);
        assert_eq!(source.image_calls.load(Ordering::Relaxed), 0);
        assert!(state.deliveries.is_empty());
    }

    #[test]
    fn status_lists_only_first_eight_targets_and_runtime_metadata() {
        let _guard = test_guard();
        replace_status(StatusSnapshot {
            plugin_enabled: true,
            worker_state: "running".to_string(),
            configured_targets: 10,
            enabled_targets: 10,
            last_poll_at: Some("2026-08-10T02:00:00Z".to_string()),
            last_poll_duration_ms: Some(42),
            last_result: "期号 2026-08-10：已入队 10，跳过 0，失败 0".to_string(),
            last_issue_date: Some("2026-08-10".to_string()),
            last_issue_hash: Some(issue_hash_prefix("issue-1")),
            target_results: (0..10)
                .map(|index| TargetResult {
                    name: format!("target-{index}"),
                    protocol: "onebot11".to_string(),
                    account_id: "bot***001".to_string(),
                    group_id: "gro***001".to_string(),
                    status: "宿主已接收入队".to_string(),
                })
                .collect(),
        });

        let output = status_text();

        assert!(output.contains("动态 API 0.6 / 配置 v1"));
        assert!(output.contains("轮询耗时：42 ms"));
        assert!(output.contains("target-7"));
        assert!(!output.contains("target-8"));
        assert!(output.contains("其余 2 个目标已省略"));
        assert!(!output.contains("Accepted"));
    }

    #[test]
    fn disabled_start_and_repeated_stop_are_idempotent() {
        let _guard = test_guard();
        let dir = tempfile::tempdir().unwrap();
        let mut disabled = config();
        disabled.enabled = false;

        for _ in 0..10 {
            start(disabled.clone(), dir.path().to_path_buf()).unwrap();
            stop().unwrap();
            stop().unwrap();
        }

        let output = status_text();
        assert!(output.contains("Worker：stopped"));
        assert!(!state_path(dir.path()).exists());
    }
}
