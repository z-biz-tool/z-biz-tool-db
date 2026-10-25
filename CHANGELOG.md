# Changelog

z-biz-tool-db 版本变更记录。格式遵循 [Conventional Commits](https://www.conventionalcommits.org/)。

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
