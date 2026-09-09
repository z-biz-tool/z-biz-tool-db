# z-biz-tool-db

> 一款现代化的跨平台数据库管理工具，参考 [Beekeeper Studio](https://www.beekeeperstudio.io/) 和 [DBeaver](https://dbeaver.io/) 的产品形态，基于 Tauri 打造，原生支持 macOS / Windows / Linux。

![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux-blue)
![Tech](https://img.shields.io/badge/Tauri-2.x-orange)
![License](https://img.shields.io/badge/license-MIT-green)

## ✨ 功能特性

### 多数据库支持

- 🐬 **MySQL** / MariaDB
- 🐘 **PostgreSQL**
- 📦 **SQLite** (本地文件)
- 🏢 **Microsoft SQL Server**

### 连接管理

- 🔌 多连接保存、编辑、删除
- 📤 **连接导出/导入** — JSON 格式（密码不含）
- 🏷️ 连接分组与命名

### SQL 编辑器

- ✏️ 语法高亮 + 一键格式化
- 🪟 **多标签页** — 同时编辑多个查询
- 💾 **查询历史** — 自动持久化最近 100 条
- ⭐ **常用查询收藏** — 带标签、备注、可搜索
- ▶️ 实时执行

### 数据浏览

- 📊 **左侧表树** — 表 + 行数 + 大小
- 🔍 **列结构查看** — 类型 / 主键 / 是否可空
- 📥 数据导出（CSV / JSON）

### 用户体验

- 🌓 亮色 / 暗色主题
- 📱 窗口大小自适应
- ⚡ 极快的启动速度（Tauri 体积优势）

---

## 🛠 技术栈

| 层 | 技术 |
|---|---|
| **桌面框架** | [Tauri 2.x](https://tauri.app/) (Rust + WebView) |
| **前端** | React 19 + TypeScript + Vite 6 |
| **UI 组件** | [Ant Design 6](https://ant.design/) |
| **状态管理** | Zustand |
| **数据库驱动** | [SQLx](https://github.com/launchbadge/sqlx) |
| **后端语言** | Rust |

---

## 🚀 开发

### 前置依赖

- Node.js 22+
- Rust stable（通过 [rustup](https://rustup.rs/) 安装）
- Tauri CLI: `cargo install tauri-cli --version "^2.0.0"`

### 启动开发服务器

```bash
# 1. 安装前端依赖
npm install

# 2. 启动 Tauri 开发模式（带热重载）
npm run tauri dev
```

应用窗口会自动启动，前端修改即时生效。

### 构建发布版本

```bash
# 本地构建当前平台
npm run tauri build

# 产物路径
src-tauri/target/release/bundle/
├── dmg/      # macOS
├── msi/      # Windows
├── deb/      # Debian / Ubuntu
└── AppImage/ # Linux 通用
```

---

## 📦 发布流程

本项目使用 `scripts/release.sh` 自动化发布：

```bash
# 1. 提交所有未提交改动
git add -A
git commit -m "feat: your changes"

# 2. 触发发布流程
bash scripts/release.sh --yes
```

脚本会：

1. 📌 将版本号 `0.1.0` → `0.2.0`（minor +1，patch 归零）
2. 🔄 同步更新 `package.json`、`Cargo.toml`、`tauri.conf.json`
3. 📤 推送 main 分支
4. 🏷️ 创建 `v0.2.0` tag 并 push
5. 🚀 GitHub Actions 自动构建 4 个平台安装包并创建 Release

支持的参数：

```bash
bash scripts/release.sh            # 交互式确认每一步
bash scripts/release.sh --yes      # 全自动（推荐 CI 使用）
bash scripts/release.sh --dry-run  # 仅预览计划，不实际执行
bash scripts/release.sh --help     # 查看帮助
```

---

## 🤝 贡献

欢迎贡献代码、报告 Bug 或提出功能建议！

1. Fork 本仓库
2. 创建 feature 分支：`git checkout -b feat/your-feature`
3. 提交改动：`git commit -m "feat: add your feature"`
4. 推送分支：`git push origin feat/your-feature`
5. 创建 Pull Request

---

## 📄 许可证

[MIT](./LICENSE) © z-biz-tool

---

## 🔗 相关项目

- [z-biz-tool-box](https://github.com/z-biz-tool/z-biz-tool-box) — 插件化工具箱（32+ 内嵌工具）
- [z-biz-tool-sys](https://github.com/z-biz-tool/z-biz-tool-sys) — 系统监控仪表盘
- [z-biz-tool-note](https://github.com/z-biz-tool/z-biz-tool-note) — 笔记工具
- [z-biz-tool-file](https://github.com/z-biz-tool/z-biz-tool-file) — 文件管理
- [z-biz-tool-terminal](https://github.com/z-biz-tool/z-biz-tool-terminal) — 终端工具
- 完整列表见 [z-biz-tool 组织主页](https://github.com/z-biz-tool)