use std::collections::BTreeSet;
use std::str::FromStr;
use std::time::Duration;

use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use url::Url;

pub const DEFAULT_FEED_URL: &str = "https://daily.juya.uk/rss.xml";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub feed_url: String,
    pub timezone: String,
    pub poll_interval_minutes: u64,
    pub request_timeout_seconds: u64,
    pub targets: Vec<TargetConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            feed_url: DEFAULT_FEED_URL.to_string(),
            timezone: "Asia/Shanghai".to_string(),
            poll_interval_minutes: 5,
            request_timeout_seconds: 15,
            targets: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TargetConfig {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub protocol: Protocol,
    pub account_id: String,
    pub group_id: String,
    #[serde(default)]
    pub image_mode: Option<ImageMode>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum Protocol {
    #[serde(rename = "onebot11")]
    OneBot11,
    #[serde(rename = "qq-official")]
    QqOfficial,
}

impl Protocol {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OneBot11 => "onebot11",
            Self::QqOfficial => "qq-official",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImageMode {
    None,
    #[default]
    Cover,
}

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub enabled: bool,
    pub feed_url: Url,
    pub timezone: Tz,
    pub poll_interval: Duration,
    pub request_timeout: Duration,
    pub targets: Vec<Target>,
}

#[derive(Clone, Debug)]
pub struct Target {
    pub name: String,
    pub enabled: bool,
    pub protocol: Protocol,
    pub account_id: String,
    pub group_id: String,
    pub image_mode: ImageMode,
}

impl Target {
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}",
            self.protocol.as_str(),
            self.account_id,
            self.group_id
        )
    }
}

fn default_true() -> bool {
    true
}

pub fn parse_and_validate(config_json: &str) -> Result<RuntimeConfig, String> {
    let config = if config_json.trim().is_empty() {
        Config::default()
    } else {
        serde_json::from_str::<Config>(config_json)
            .map_err(|error| format!("配置 JSON 无效：{error}"))?
    };

    if !(1..=1440).contains(&config.poll_interval_minutes) {
        return Err("poll_interval_minutes 必须在 1 到 1440 之间".to_string());
    }
    if !(3..=60).contains(&config.request_timeout_seconds) {
        return Err("request_timeout_seconds 必须在 3 到 60 之间".to_string());
    }
    if config.targets.len() > 50 {
        return Err("targets 最多允许 50 项".to_string());
    }

    let feed_url_value = required_trimmed_max(&config.feed_url, "feed_url", 2048)?;
    let feed_url =
        Url::parse(&feed_url_value).map_err(|error| format!("feed_url 不是有效 URL：{error}"))?;
    if feed_url.scheme() != "https" {
        return Err("feed_url 必须使用 HTTPS".to_string());
    }
    if !feed_url.username().is_empty() || feed_url.password().is_some() {
        return Err("feed_url 不能包含用户名或密码".to_string());
    }

    let timezone_value = required_trimmed_max(&config.timezone, "timezone", 64)?;
    let timezone = Tz::from_str(&timezone_value)
        .map_err(|_| format!("timezone 不是有效 IANA 时区：{timezone_value}"))?;

    let mut keys = BTreeSet::new();
    let mut targets = Vec::with_capacity(config.targets.len());
    for (index, target) in config.targets.into_iter().enumerate() {
        let name = required_trimmed_max(&target.name, &format!("targets[{index}].name"), 64)?;
        let account_id = required_trimmed_max(
            &target.account_id,
            &format!("targets[{index}].account_id"),
            128,
        )?;
        let group_id =
            required_trimmed_max(&target.group_id, &format!("targets[{index}].group_id"), 256)?;

        let image_mode = match target.protocol {
            Protocol::OneBot11 => target.image_mode.unwrap_or_default(),
            Protocol::QqOfficial => {
                if target.image_mode.is_some() {
                    return Err(format!(
                        "targets[{index}] 是 QQ 官方目标，不能配置 image_mode"
                    ));
                }
                ImageMode::None
            }
        };

        let key = format!("{}|{account_id}|{group_id}", target.protocol.as_str());
        if !keys.insert(key) {
            return Err(format!(
                "targets[{index}] 与前面的目标指向同一协议、账号和群"
            ));
        }

        targets.push(Target {
            name,
            enabled: target.enabled,
            protocol: target.protocol,
            account_id,
            group_id,
            image_mode,
        });
    }

    Ok(RuntimeConfig {
        enabled: config.enabled,
        feed_url,
        timezone,
        poll_interval: Duration::from_secs(config.poll_interval_minutes * 60),
        request_timeout: Duration::from_secs(config.request_timeout_seconds),
        targets,
    })
}

fn required_trimmed(value: &str, field: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        Err(format!("{field} 不能为空"))
    } else {
        Ok(value.to_string())
    }
}

fn required_trimmed_max(value: &str, field: &str, max_chars: usize) -> Result<String, String> {
    let value = required_trimmed(value, field)?;
    if value.chars().count() > max_chars {
        return Err(format!("{field} 最多允许 {max_chars} 个字符"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_safe_and_disabled() {
        let config = parse_and_validate("").unwrap();
        assert!(!config.enabled);
        assert!(config.targets.is_empty());
        assert_eq!(config.feed_url.as_str(), DEFAULT_FEED_URL);
    }

    #[test]
    fn trims_string_ids_without_parsing_them() {
        let config = parse_and_validate(
            r#"{
                "enabled": true,
                "feed_url": "https://daily.juya.uk/rss.xml",
                "timezone": "Asia/Shanghai",
                "poll_interval_minutes": 5,
                "request_timeout_seconds": 15,
                "targets": [{
                    "name": " test ",
                    "enabled": true,
                    "protocol": "onebot11",
                    "account_id": " 00123 ",
                    "group_id": " 00456 ",
                    "image_mode": "none"
                }]
            }"#,
        )
        .unwrap();

        assert_eq!(config.targets[0].name, "test");
        assert_eq!(config.targets[0].account_id, "00123");
        assert_eq!(config.targets[0].group_id, "00456");
    }

    #[test]
    fn rejects_duplicate_targets() {
        let error = parse_and_validate(
            r#"{
                "enabled": true,
                "feed_url": "https://daily.juya.uk/rss.xml",
                "timezone": "Asia/Shanghai",
                "poll_interval_minutes": 5,
                "request_timeout_seconds": 15,
                "targets": [
                    {"name":"a","enabled":true,"protocol":"qq-official","account_id":"app","group_id":"g"},
                    {"name":"b","enabled":true,"protocol":"qq-official","account_id":"app","group_id":"g"}
                ]
            }"#,
        )
        .unwrap_err();

        assert!(error.contains("targets[1]"));
    }

    #[test]
    fn rejects_official_image_mode() {
        let error = parse_and_validate(
            r#"{
                "enabled": true,
                "feed_url": "https://daily.juya.uk/rss.xml",
                "timezone": "Asia/Shanghai",
                "poll_interval_minutes": 5,
                "request_timeout_seconds": 15,
                "targets": [{
                    "name":"a","enabled":true,"protocol":"qq-official",
                    "account_id":"app","group_id":"g","image_mode":"cover"
                }]
            }"#,
        )
        .unwrap_err();

        assert!(error.contains("不能配置 image_mode"));
    }

    #[test]
    fn rejects_non_https_feed() {
        let error = parse_and_validate(
            r#"{
                "enabled": false,
                "feed_url": "http://daily.juya.uk/rss.xml",
                "timezone": "Asia/Shanghai",
                "poll_interval_minutes": 5,
                "request_timeout_seconds": 15,
                "targets": []
            }"#,
        )
        .unwrap_err();
        assert_eq!(error, "feed_url 必须使用 HTTPS");
    }

    #[test]
    fn parses_complete_mixed_configuration() {
        let config = parse_and_validate(
            r#"{
                "enabled": true,
                "feed_url": "https://daily.juya.uk/rss.xml",
                "timezone": "Asia/Shanghai",
                "poll_interval_minutes": 1440,
                "request_timeout_seconds": 60,
                "targets": [
                    {"name":"onebot","enabled":true,"protocol":"onebot11","account_id":"001","group_id":"002","image_mode":"cover"},
                    {"name":"official","enabled":false,"protocol":"qq-official","account_id":"app","group_id":"openid"}
                ]
            }"#,
        )
        .unwrap();

        assert!(config.enabled);
        assert_eq!(config.poll_interval, Duration::from_secs(1440 * 60));
        assert_eq!(config.request_timeout, Duration::from_secs(60));
        assert_eq!(config.targets.len(), 2);
        assert_eq!(config.targets[0].image_mode, ImageMode::Cover);
        assert_eq!(config.targets[1].protocol, Protocol::QqOfficial);
    }

    #[test]
    fn rejects_interval_and_timeout_outside_boundaries() {
        for (field, value, expected) in [
            ("poll_interval_minutes", 0, "poll_interval_minutes"),
            ("poll_interval_minutes", 1441, "poll_interval_minutes"),
            ("request_timeout_seconds", 2, "request_timeout_seconds"),
            ("request_timeout_seconds", 61, "request_timeout_seconds"),
        ] {
            let mut value_json = serde_json::json!({
                "enabled": false,
                "feed_url": DEFAULT_FEED_URL,
                "timezone": "Asia/Shanghai",
                "poll_interval_minutes": 5,
                "request_timeout_seconds": 15,
                "targets": []
            });
            value_json[field] = serde_json::json!(value);
            let error = parse_and_validate(&value_json.to_string()).unwrap_err();
            assert!(error.contains(expected));
        }
    }

    #[test]
    fn rejects_invalid_timezone_and_unknown_fields() {
        let invalid_timezone = r#"{
            "enabled": false,
            "feed_url": "https://daily.juya.uk/rss.xml",
            "timezone": "Mars/Base",
            "poll_interval_minutes": 5,
            "request_timeout_seconds": 15,
            "targets": []
        }"#;
        assert!(
            parse_and_validate(invalid_timezone)
                .unwrap_err()
                .contains("IANA 时区")
        );

        let unknown_root = invalid_timezone.replace(
            "\"timezone\": \"Mars/Base\"",
            "\"timezone\": \"Asia/Shanghai\", \"unexpected\": true",
        );
        assert!(
            parse_and_validate(&unknown_root)
                .unwrap_err()
                .contains("unknown field")
        );
    }

    #[test]
    fn rejects_feed_credentials_and_overlong_target_fields() {
        let credential_error = parse_and_validate(
            r#"{
                "enabled": false,
                "feed_url": "https://user:pass@daily.juya.uk/rss.xml",
                "timezone": "Asia/Shanghai",
                "poll_interval_minutes": 5,
                "request_timeout_seconds": 15,
                "targets": []
            }"#,
        )
        .unwrap_err();
        assert!(credential_error.contains("用户名或密码"));

        let config = serde_json::json!({
            "enabled": true,
            "feed_url": DEFAULT_FEED_URL,
            "timezone": "Asia/Shanghai",
            "poll_interval_minutes": 5,
            "request_timeout_seconds": 15,
            "targets": [{
                "name": "x".repeat(65),
                "enabled": true,
                "protocol": "onebot11",
                "account_id": "bot",
                "group_id": "group",
                "image_mode": "none"
            }]
        });
        assert!(
            parse_and_validate(&config.to_string())
                .unwrap_err()
                .contains("64 个字符")
        );
    }
}
