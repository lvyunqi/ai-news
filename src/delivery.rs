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
    use std::ffi::c_void;
    use std::sync::Mutex;

    use abi_stable_host_api::{
        HOST_API_V1_ABI_VERSION, HostApiV1, PROACTIVE_BOT_ACCOUNT_SELECTOR_PREFIX,
        ProactiveSendRequest, bind_host_api_v1, unbind_host_api_v1,
    };

    use super::*;

    static HOST_TEST_LOCK: Mutex<()> = Mutex::new(());

    unsafe extern "C" fn record_send(
        context: *mut c_void,
        request: *const ProactiveSendRequest,
    ) -> i32 {
        if context.is_null() || request.is_null() {
            return SendEnqueueStatus::InvalidRequest.code();
        }
        // SAFETY: BoundHost keeps the allocation alive until the API is unbound.
        let output = unsafe { &*(context.cast::<Mutex<Vec<ProactiveSendRequest>>>()) };
        // SAFETY: The Host API guarantees a valid request pointer for the callback duration.
        let request = unsafe { &*request };
        match output.lock() {
            Ok(mut output) => {
                output.push(request.clone());
                SendEnqueueStatus::Accepted.code()
            }
            Err(_) => SendEnqueueStatus::HostUnavailable.code(),
        }
    }

    struct BoundHost {
        output: Box<Mutex<Vec<ProactiveSendRequest>>>,
    }

    impl BoundHost {
        fn new() -> Self {
            let _ = unbind_host_api_v1();
            let output = Box::new(Mutex::new(Vec::new()));
            let context = (&*output as *const Mutex<Vec<ProactiveSendRequest>>)
                .cast_mut()
                .cast::<c_void>();
            let api = HostApiV1 {
                abi_version: HOST_API_V1_ABI_VERSION,
                context,
                enqueue_send: Some(record_send),
            };
            // SAFETY: The callback and output allocation remain valid until Drop unbinds the API.
            assert_eq!(
                unsafe { bind_host_api_v1(&api) },
                SendEnqueueStatus::Accepted.code()
            );
            Self { output }
        }

        fn requests(&self) -> Vec<ProactiveSendRequest> {
            self.output.lock().expect("host output").clone()
        }
    }

    impl Drop for BoundHost {
        fn drop(&mut self) {
            assert_eq!(unbind_host_api_v1(), SendEnqueueStatus::Accepted.code());
        }
    }

    fn target(protocol: Protocol, account_id: &str, group_id: &str) -> Target {
        Target {
            name: "contract".to_string(),
            enabled: true,
            protocol,
            account_id: account_id.to_string(),
            group_id: group_id.to_string(),
            image_mode: crate::config::ImageMode::None,
        }
    }

    fn content() -> PreparedContent {
        PreparedContent {
            onebot_text: "OneBot text".to_string(),
            qq_markdown: "# QQ Markdown\n\ncontent".to_string(),
            cover_url: None,
        }
    }

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

    #[test]
    fn onebot_host_contract_uses_account_group_and_image_then_text() {
        let _lock = HOST_TEST_LOCK.lock().expect("host test lock");
        let host = BoundHost::new();
        let target = target(Protocol::OneBot11, "00123", "00456");

        let status = HostDeliverySender.send(&target, &content(), Some("YWJj"));

        assert_eq!(status, SendEnqueueStatus::Accepted);
        let requests = host.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(
            request.bot_id.as_str(),
            format!("{PROACTIVE_BOT_ACCOUNT_SELECTOR_PREFIX}00123")
        );
        assert_eq!(request.target_kind.as_str(), "group");
        assert_eq!(request.target_id.as_str(), "00456");
        assert!(request.message.is_empty());
        let segments: serde_json::Value =
            serde_json::from_str(request.segments_json.as_str()).unwrap();
        let segments = segments.as_array().unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0]["type"], "image");
        assert_eq!(segments[0]["data"]["file"], "base64://YWJj");
        assert_eq!(segments[1]["type"], "text");
        assert_eq!(segments[1]["data"]["text"], "OneBot text");
    }

    #[test]
    fn official_host_contract_uses_group_openid_and_only_markdown() {
        let _lock = HOST_TEST_LOCK.lock().expect("host test lock");
        let host = BoundHost::new();
        let target = target(Protocol::QqOfficial, "102012345", "0FC3F8C45E7A-openid");

        let status = HostDeliverySender.send(&target, &content(), None);

        assert_eq!(status, SendEnqueueStatus::Accepted);
        let requests = host.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(
            request.bot_id.as_str(),
            format!("{PROACTIVE_BOT_ACCOUNT_SELECTOR_PREFIX}102012345")
        );
        assert_eq!(request.target_kind.as_str(), "group");
        assert_eq!(request.target_id.as_str(), "0FC3F8C45E7A-openid");
        assert!(request.message.is_empty());
        assert_eq!(request.context_json.as_str(), "{}");
        let segments: serde_json::Value =
            serde_json::from_str(request.segments_json.as_str()).unwrap();
        let segments = segments.as_array().unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0]["type"], "markdown");
        assert_eq!(segments[0]["data"]["content"], "# QQ Markdown\n\ncontent");
    }
}
