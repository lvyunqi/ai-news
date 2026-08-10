# QimenBot AI News

QimenBot API 0.6 动态插件，轮询[橘鸦 AI 早报](https://daily.juya.uk/rss.xml)，主动推送到 OneBot 11 或 QQ 官方机器人群。

设计与实施规格位于 <https://github.com/lvyunqi/ai-news-design>。

## 功能

- QimenBot Web 管理面板在线配置。
- 多机器人、多群目标，按配置顺序串行推送。
- OneBot 11：纯文本概览，可选 Base64 封面图。
- QQ 官方机器人：自定义 Markdown 全文，公网 HTTPS 图片直接插入。
- 按 `protocol + account_id + group_id + issue_id` 去重。
- 配置 reload、宿主重启后保留去重状态。
- 后台线程在动态库卸载前停止并 `join`。
- 管理员命令 `ainews status` 查看运行状态。

## 版本要求

| 项目 | 版本 |
| --- | --- |
| QimenBot | `v0.1.18` 或更高 |
| 动态 ABI | `0.6` |
| `abi-stable-host-api` | `0.1.13` |
| `qimen-dynamic-plugin-derive` | `0.1.13` |
| Rust | `1.89` 或更高 |

插件 ID 固定为 `ai-news`，配置文件默认为 `config/plugins/ai-news.toml`。

## 宿主配置

插件不保存 Bot 凭据。先在 QimenBot 宿主中配置 Bot，并为每个启用 Bot 设置稳定且唯一的 `account_id`。

OneBot 通常使用 Bot 的 `self_id`：

```toml
[[bots]]
id = "onebot-main"
account_id = "2733944636"
protocol = "onebot11"
enabled = true
```

QQ 官方机器人建议使用 AppID：

```toml
[[bots]]
id = "qq-official-main"
account_id = "102012345"
protocol = "qq-official"
enabled = true
```

QQ AppSecret、Access Token、OneBot Token 和连接地址仍由 QimenBot 宿主管理。

## 插件配置

安装动态库后，在 QimenBot Web 插件页重新扫描并打开 `ai-news` 的配置入口。示例：

```toml
enabled = true
feed_url = "https://daily.juya.uk/rss.xml"
timezone = "Asia/Shanghai"
poll_interval_minutes = 5
request_timeout_seconds = 15

[[targets]]
name = "OneBot AI 群"
enabled = true
protocol = "onebot11"
account_id = "2733944636"
group_id = "123456789"
image_mode = "cover"

[[targets]]
name = "官方机器人 AI 群"
enabled = true
protocol = "qq-official"
account_id = "102012345"
group_id = "0FC3F8C45E7A..."
```

ID 始终按字符串处理：

- OneBot `group_id` 是普通群号。
- QQ 官方 `group_id` 必须填写开放平台提供的 `group_openid`，不是普通 QQ 群号。
- OneBot `image_mode` 支持 `none` 和 `cover`。
- QQ 官方目标固定使用 Markdown，不接受 `image_mode`。

在线配置保存采用 `reload`：宿主备份并原子写入配置，旧插件停止 worker，新实例读取相同去重状态后启动。

## 推送行为

插件启动后立即检查一次 RSS，之后按固定间隔轮询。只选择配置时区当天最新一期，不补发历史内容。

OneBot 消息包含日期、概览分类、新闻标题、原始链接和网页全文入口。封面图会先下载到内存，同时校验响应 `Content-Type` 与文件魔数，再转为 Base64；插件不会把本地路径或 `file://` 交给宿主。

QQ 官方机器人使用单个 Markdown 消息段。图片保留 RSS 中的公网 HTTPS URL，由 QQ 开放平台下载转存；插件不调用 QQ 媒体上传接口。

Host API 返回 `Accepted` 只表示 QimenBot 宿主已接收入队，不代表协议平台最终送达。平台权限、机器人审核状态、主动消息开关和频控仍可能导致发送失败。

## 状态命令

管理员执行：

```text
/ainews status
```

命令只读取内存快照，不访问 RSS、不下载图片、不发送测试消息。输出包括插件/API/配置版本、worker 状态、目标数量、最近轮询耗时、期号 ID 哈希前缀和最多 8 个目标的最近入队状态。
每个目标结果同时包含脱敏账号、脱敏群标识、Host API 状态和本轮记录时间。

## 构建

```bash
cargo fmt --check
cargo check --locked --all-targets
cargo test --locked --all-targets
cargo build --locked --release
```

产物：

| 宿主 | target | 产物 |
| --- | --- | --- |
| Windows x64 | `x86_64-pc-windows-msvc` | `target/release/qimen_dynamic_plugin_ai_news.dll` |
| Linux x64 GNU | `x86_64-unknown-linux-gnu` | `target/release/libqimen_dynamic_plugin_ai_news.so` |
| Linux ARM64 GNU | `aarch64-unknown-linux-gnu` | `target/release/libqimen_dynamic_plugin_ai_news.so` |

将动态库复制到 QimenBot `plugin_bin_dir`，默认是 `plugins/bin/`，然后在 Web 插件页点击重新扫描。动态库必须匹配宿主操作系统、CPU 和 C 运行时；musl 宿主不支持动态加载。

## 候选版本发布

推送与 `Cargo.toml` 版本一致的 `v*` tag 会触发 Release 工作流。工作流先执行格式、Clippy 和全部测试，再构建以下固定资产：

- `qimen_dynamic_plugin_ai_news-x86_64-pc-windows-msvc.dll`
- `libqimen_dynamic_plugin_ai_news-x86_64-unknown-linux-gnu.so`
- `libqimen_dynamic_plugin_ai_news-aarch64-unknown-linux-gnu.so`

每个动态库同时生成 `.sha256` 和 `.json` 元数据。Linux 在 Debian 11 容器中原生构建，并把 `file`、`ldd`、`readelf --version-info` 结果及实际最高 GLIBC 符号版本写入发布资产。发布前必须先完成目标 QimenBot 宿主加载和真实机器人能力确认；工作流配置完成不等于协议已经验收。

## 依赖策略

Dependabot 每周检查 Cargo 依赖和 GitHub Actions。依赖升级仍必须通过格式、Clippy、全部测试和 release 构建；发布前额外检查 RustSec 公告及新增依赖许可证。项目接受 Apache-2.0、MIT、BSD、ISC、Unicode、Zlib 等常见兼容许可证；GPL/AGPL、未知许可证、私有 registry、本地 path 和浮动 Git 分支需要维护者明确审查，不能自动合并。

## 状态文件

去重状态写入插件 `data_dir/delivery-state.json`，不属于在线配置。状态文件损坏或写入失败时，插件进入保护状态并暂停自动推送，避免把当天早报重复刷到群里。

不要提交：

- `config/plugins/ai-news.toml`
- `delivery-state.json`
- `plugins/bin/`
- 日志、数据库、Bot 凭据和构建后的动态库

## 已知限制

- v1 只保证兼容橘鸦 AI 早报的 RSS 结构。
- 只支持群聊，不支持私聊、频道和 DMS。
- 没有 Cron、历史补发、手动推送或聊天内修改配置。
- OneBot 只发送概览和最多一张封面，不发送全文图片集。
- QQ 官方 Markdown 的真实显示和主动消息权限必须在目标机器人与客户端实测。
- URL 检查会拒绝显式私网 IP 和逐跳危险重定向，但域名 DNS 重绑定仍依赖部署网络策略防护。

## License

Apache-2.0
