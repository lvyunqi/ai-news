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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdminCommand {
    Status,
    Push { force: bool },
    Usage,
}

fn parse_admin_command(args: &str) -> AdminCommand {
    let parts = args.split_whitespace().collect::<Vec<_>>();
    match parts.as_slice() {
        [] => AdminCommand::Status,
        [command] if command.eq_ignore_ascii_case("status") => AdminCommand::Status,
        [command] if command.eq_ignore_ascii_case("push") => AdminCommand::Push { force: false },
        [command, flag]
            if command.eq_ignore_ascii_case("push") && flag.eq_ignore_ascii_case("--force") =>
        {
            AdminCommand::Push { force: true }
        }
        _ => AdminCommand::Usage,
    }
}

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
        description = "查看状态或立即推送最新一期 AI 早报",
        category = "tools",
        role = "admin",
        scope = "all"
    )]
    fn command(request: &CommandRequest) -> CommandResponse {
        match parse_admin_command(request.args.as_str()) {
            AdminCommand::Status => CommandResponse::text(&runtime::status_text()),
            AdminCommand::Push { force } => command_result(runtime::request_push(force)),
            AdminCommand::Usage => {
                CommandResponse::text("用法：ainews status | ainews push [--force]")
            }
        }
    }

    fn command_result(result: Result<String, String>) -> CommandResponse {
        match result {
            Ok(message) => CommandResponse::text(&message),
            Err(error) => CommandResponse::text(&format!("无法执行：{error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_and_manual_push_commands() {
        assert_eq!(parse_admin_command(""), AdminCommand::Status);
        assert_eq!(parse_admin_command("STATUS"), AdminCommand::Status);
        assert_eq!(
            parse_admin_command("push"),
            AdminCommand::Push { force: false }
        );
        assert_eq!(
            parse_admin_command("  PUSH   --FORCE  "),
            AdminCommand::Push { force: true }
        );
        assert_eq!(parse_admin_command("push now"), AdminCommand::Usage);
        assert_eq!(
            parse_admin_command("push --force extra"),
            AdminCommand::Usage
        );
    }
}
