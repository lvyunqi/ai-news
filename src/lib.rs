//! QimenBot AI daily news proactive push plugin.

mod config;
mod delivery;
mod feed;
mod media;
mod render;
mod runtime;
mod state;

use std::path::PathBuf;

use abi_stable_host_api::{
    CommandRequest, CommandResponse, PluginConfigRequest, PluginConfigResult, PluginInitConfig,
    PluginInitResult,
};
use qimen_dynamic_plugin_derive::dynamic_plugin;

#[dynamic_plugin(
    id = "ai-news",
    version = "0.1.0",
    api = "0.6",
    config_schema = "../config.schema.json",
    config_ui = "../config.ui.json",
    config_version = 1,
    config_apply = "reload"
)]
mod plugin {
    use super::*;

    #[init]
    fn on_init(init: PluginInitConfig) -> PluginInitResult {
        let config = match config::parse_and_validate(init.config_json.as_str()) {
            Ok(config) => config,
            Err(error) => return PluginInitResult::err(&error),
        };
        let data_dir = PathBuf::from(init.data_dir.as_str());
        match runtime::start(config, data_dir) {
            Ok(()) => PluginInitResult::ok(),
            Err(error) => PluginInitResult::err(&error),
        }
    }

    #[validate_config]
    fn on_validate_config(request: &PluginConfigRequest) -> PluginConfigResult {
        match config::parse_and_validate(request.config_json.as_str()) {
            Ok(_) => PluginConfigResult::ok(),
            Err(error) => PluginConfigResult::err(&error),
        }
    }

    #[shutdown]
    fn on_shutdown() {
        if let Err(error) = runtime::stop() {
            eprintln!("[ai-news][shutdown] {error}");
        }
    }

    #[command(
        name = "ainews",
        description = "查看 AI 早报主动推送状态",
        category = "tools",
        role = "admin",
        scope = "all"
    )]
    fn status(request: &CommandRequest) -> CommandResponse {
        let args = request.args.as_str().trim();
        if args.is_empty() || args.eq_ignore_ascii_case("status") {
            CommandResponse::text(&runtime::status_text())
        } else {
            CommandResponse::text("用法：ainews status")
        }
    }
}
