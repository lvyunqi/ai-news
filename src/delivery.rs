use abi_stable_host_api::{BotApi, SendBuilder, SendEnqueueStatus};

use crate::config::{Protocol, Target};
use crate::render::PreparedContent;

pub trait DeliverySender: Send + Sync {
    fn send(
        &self,
        target: &Target,
        content: &PreparedContent,
        cover_base64: Option<&str>,
    ) -> SendEnqueueStatus;
}

pub struct HostDeliverySender;

impl DeliverySender for HostDeliverySender {
    fn send(
        &self,
        target: &Target,
        content: &PreparedContent,
        cover_base64: Option<&str>,
    ) -> SendEnqueueStatus {
        match target.protocol {
            Protocol::OneBot11 => {
                let mut builder =
                    SendBuilder::group(&target.group_id).bot_account(&target.account_id);
                if let Some(cover) = cover_base64 {
                    builder = builder.image_base64(cover);
                }
                builder.text(&content.onebot_text).try_send()
            }
            Protocol::QqOfficial => {
                let segments = official_segments_json(&content.qq_markdown);
                BotApi::for_account(&target.account_id).send_rich(
                    "group",
                    &target.group_id,
                    "{}",
                    &segments,
                )
            }
        }
    }
}

pub fn official_segments_json(markdown: &str) -> String {
    serde_json::json!([{
        "type": "markdown",
        "data": {
            "content": markdown
        }
    }])
    .to_string()
}

pub fn status_name(status: SendEnqueueStatus) -> &'static str {
    match status {
        SendEnqueueStatus::Accepted => "Accepted",
        SendEnqueueStatus::HostUnavailable => "HostUnavailable",
        SendEnqueueStatus::InvalidRequest => "InvalidRequest",
        SendEnqueueStatus::BotNotFound => "BotNotFound",
        SendEnqueueStatus::BotDisabled => "BotDisabled",
        SendEnqueueStatus::QueueFull => "QueueFull",
        SendEnqueueStatus::HostShuttingDown => "HostShuttingDown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_payload_has_only_one_markdown_segment() {
        let markdown = r#"# Title
content "quoted"\path"#;
        let value: serde_json::Value =
            serde_json::from_str(&official_segments_json(markdown)).unwrap();
        let segments = value.as_array().unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0]["type"], "markdown");
        assert_eq!(segments[0]["data"]["content"], markdown);
        assert!(segments[0].get("text").is_none());
    }

    #[test]
    fn every_enqueue_status_has_a_stable_name() {
        let cases = [
            (SendEnqueueStatus::Accepted, "Accepted"),
            (SendEnqueueStatus::HostUnavailable, "HostUnavailable"),
            (SendEnqueueStatus::InvalidRequest, "InvalidRequest"),
            (SendEnqueueStatus::BotNotFound, "BotNotFound"),
            (SendEnqueueStatus::BotDisabled, "BotDisabled"),
            (SendEnqueueStatus::QueueFull, "QueueFull"),
            (SendEnqueueStatus::HostShuttingDown, "HostShuttingDown"),
        ];

        for (status, expected) in cases {
            assert_eq!(status_name(status), expected);
        }
    }
}
