# Changelog

z-biz-tool-db 版本变更记录。格式遵循 [Conventional Commits](https://www.conventionalcommits.org/)。

## [已废弃] - 2026-11-06

**本仓废弃，能力并入 z-biz-tool-file。**

数据库管理成为 z-biz-tool-file 的第四个板块（活动栏「图书馆」下方），
独立 app 不再出包、不再单独发版，能力只维护一份。

搬过去的东西：

- 后端 21840 行 Rust（SQLx 四驱动、报表算子链、AI SQL 起草与本机校验、
  连接池生命周期、报表簿存取）→ `z-biz-tool-file/src-tauri/src/db_app/`
- 前端 9300 行（连接列表、SQL 编辑器、表树、报表工作台、Agent 面板）
  → `z-biz-tool-file/src/apps/db/`

合并时做的三处改动（与本仓原实现有意的差异）：

- **数据库密码不再进 macOS 钥匙串**。本仓原本用 `keyring` crate 存密码，
  而 macOS 钥匙串的 ACL 绑在代码签名上 —— app 每次重打包 cdhash 就变，
  每次读密码都弹一次授权框。改用 `MultiSecretFile` 自建加密文件
  （`~/.z-biz-tool/db/secrets.enc`），不再触发系统弹窗。
- **LLM 配置双副本收敛成一份**。本仓有一份 `llm_shared.rs`，z-biz-tool-file
  也有自己的 `llm_config.rs`；合并后统一走 file 那份，db 侧的三个 API
  （`get_ai_config_status` / `current_cap` 等）补进 file 的 `llm_config.rs`。
- **提示条与确认框统一到 file 的 antd `<App>` context**。本仓原先用
  `message.useMessage()` / `Modal.useModal()` 各自搭 holder，静态 API 吃不到
  主题（深色下弹白底黑字），现在整块换成 `App.useApp()`。

本仓转为只读历史存档。**功能改动请去 z-biz-tool-file。**

## [0.2.0] - 2026-10-25

自 v0.1.0（2026-08-03）以来共 25 个提交：7 个 feat、8 个 fix，测试全绿（Rust 侧 23 个测试模块 + TS 契约/门禁测试网）。版本号从 0.1.0 跳到 0.2.0：功能与质量均已跨过 minor 门槛，而版本账自 8 月起停更（详见 lead `doc/分发/版本账失序清单.md`）。

### Features

- 凭据存储接入 OS Keychain —— 后端可配、默认 auto，密码不再裸存
- 功能与交互轮：补 CSV 导入门禁、修跨语言摘要不一致、新增三道尺子
- 三个 TLS no-op 分支接上 sqlx 真实 SSL 模式，并修掉一个安全缺口
- 从未接线的 API 骨架接进生产路径；clippy 收严为 `-D warnings`
- 收编 T-058 安全加固 + cargo target 产物探测；报表台与 Agent 面板按需装载（首屏）

### Fixes

- CSP 从 null 改为分指令白名单（全组织最后一个 null CSP 仓）
- 修「装了个寂寞还报喜」——安装成功判定改为可测代码
- 摘掉死依赖 z-biz-tool-shared（公网不存在该包，源码零引用）
- eprintln 多余借用（clippy useless_borrows_in_formatting）
- 标准库常量替掉手写近似值（clippy approx_constant）
- lockfile 根条目漏声明 @codemirror/autocomplete
- 解除 npmmirror 地域锁定；cargo test 开 `--nocapture` 显示跳过标记

### Chore / CI

- MIT 许可证统一；图标挂进 build script 重跑指纹；换家族矢量图标源
- rustfmt 405 处清零并把 fmt 变成门禁；`tests/*.test.ts` 接进流水线
- release.sh 换用 lead 模板版，本仓特异门禁拆入 release-gate.sh
