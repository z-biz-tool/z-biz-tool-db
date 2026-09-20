# Agent 集成指南

## 概述

`z-biz-tool-db` 现在可以使用 `z-biz-tool-shared` 中的共享 Agent 组件。

## 集成步骤

### 1. 安装依赖

```bash
cd z-biz-tool-db
npm install z-biz-tool-shared
```

### 2. 在组件中使用 Agent

```tsx
import { AgentPanel } from 'z-biz-tool-shared';
import { useAgentStore } from 'z-biz-tool-shared';

// 在组件中使用 AgentPanel
<AgentPanel />

// 或者使用 AgentStore
const { query, optimize, analyze, diagnoseError } = useAgentStore();
```

### 3. 配置 Agent

Agent 使用 Zustand 状态管理，可以在应用启动时配置：

```tsx
import { useAgentStore } from 'z-biz-tool-shared';

// 配置 Agent
useAgentStore.getState().setContext({
  connectionId: 'db-1',
  databaseType: 'mysql',
  databaseName: 'testdb',
  tables: [...],
});
```

## 共享组件列表

### Agent 模块 (`src/agent/`)

- **AgentPanel** - 完整的 Agent 交互面板（消息列表 + 输入框）
- **AgentManager** - Zustand 状态管理（query/optimize/analyze/diagnoseError）
- **QueryHistoryViewer** - 查询历史查看器
- **SQLEditor** - 带语法高亮的 SQL 编辑器

## 与 Rust Agent 的区别

| 特性 | Rust Agent (已移除) | Shared Agent (新) |
|------|-------------------|------------------|
| 语言 | Rust | TypeScript/React |
| 部署 | 本地微服务 | NPM 包 |
| UI | 需要 Tauri | React 组件 |
| 状态 | SQLite | Zustand |
| 学习成本 | 需要 Rust 知识 | 标准 React |

## 迁移指南

### 旧代码 (Rust Agent)

```tsx
// 调用 Rust 后端的 Agent 命令
const result = await invoke("agent_query_suggest", {
  natural_language: "查询所有用户",
  config: dbConfig,
});
```

### 新代码 (Shared Agent)

```tsx
// 使用共享的 Agent 组件
import { AgentPanel } from 'z-biz-tool-shared';

// 直接在 UI 中使用
<AgentPanel />

// 或者使用状态管理
const { query } = useAgentStore();
const result = await query("查询所有用户");
```

## 优势

1. ✅ **零配置** - 不需要启动额外的 Agent 服务
2. ✅ **一体化** - UI 和逻辑都在同一个 React 应用中
3. ✅ **易于调试** - 标准 React 开发流程
4. ✅ **可复用** - 所有 z-biz-tool 项目共享

## 下一步

1. 安装 z-biz-tool-shared 依赖
2. 替换现有的 AI 面板为 AgentPanel
3. 移除 Rust 后端的 Agent 命令
4. 清理相关依赖
