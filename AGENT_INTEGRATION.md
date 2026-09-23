# Agent 对话接缝说明

## 一句话

Agent 不是一条后端命令，也不是一场"本地微服务"。它是前端的一对组件 + 宿主应用登记的
处理函数；真正干活的是 `ai_*` 那几条走 HTTP 到大模型的命令。

## 组成

- `src/agent/types.ts`：`AgentIntent`（`query` / `diagnose` / `report`）、`AgentMessage`、`AgentResponse`、
  `AgentCrossDb`（本机挡下的表其实在别的连接里）、`ChainOutcome`（报表那条链跑到哪一步收的口）。
- `src/agent/AgentManager.ts`：zustand store，只存 `messages` / `busy` / `handler`。
  `ask()` 原样回显用户这一轮，然后调用 handler；没登记 handler 就明确回"没有可用的后端实现"，
  不编造回答。
- `src/agent/AgentPanel.tsx`：纯展示 + 输入。它不认识任何 Tauri 命令名。
- `src/App.tsx`：宿主。`useEffect` 里 `setHandler(askAgent)` 登记一个稳定转发（内部读
  `agentAskRef.current`，因为 `agentAsk` 每次渲染都是新闭包，要读当前编辑器内容和勾选的表）。

## 两条意图实际走哪里

| 意图 | 输入 | 落到 |
|------|------|------|
| `query` | 自然语言 + 本机现建的表目录（含真实列清单） | `ai_sql_generate`，产出先过本机列校验，最多带打回次数 |
| `diagnose` | 报错文本 + 编辑器里那条 SQL | `ai_diagnose_error` |
| `report` | 自然语言 | 报表那条链：`ai_report_pick_tables` → `ai_report_draft` → `report_view_render`。跑在报表工作台（挂在背后），结果由 `ChainOutcome` 回话：没出图就明说卡在哪一步 |

`query` 用的表目录由 `App.tsx` 的 `buildAiCatalog` 现建，和"生成 SQL"页共用同一个实现：
读不到列清单的表直接丢进 `failed`，不带着空清单去问模型。

## 跨库这件事在哪一侧

`query` 这条腿只能拿到**当前连接**的表目录：一条 SQL 进不了两个库。撞上了（本机挡下的表
其实在别的连接里）错误卡与气泡会点名它在哪个连接，并给一键把那句需求送去报表工作台跑完整条链
（`App.goReport(q, run, done, switchMode)`）。报表那条腿才是跨库的：各库分别取数、在本机内存里 join。

## 边界

- Agent 不碰数据库执行句柄：它只产草稿。填进编辑器要点一下「填进编辑器」，执行由用户走常规授权。
- 上下文只有一处真源：当前 `selectedConnection` + 勾选的表。store 里不再存 `config/context` 副本。
- 未配置 AI 参数时直接弹配置窗口并说明原因，不发请求。

## 历史：删掉了什么

T-074 删除了这套死接口面——四条 `invoke("agent_*")` 命令（`agent_query_suggest` /
`agent_sql_optimize` / `agent_results_analyze` / `agent_error_diagnose`）、`call_agent_service`
（固定 POST 到本机 `http://localhost:8787`，而这个服务在本仓库里从来不存在），以及从未注册进
`invoke_handler` 的 `src-tauri/src/agent_service.rs`（`agent_assist_v2` 恒返回
`AGENT_SERVICE_UNAVAILABLE`）。它们在 `src/**` 里零调用者，却要接收带凭据的 `DBConfig`、
还会为"取表清单"真去连库，是纯增加攻击面的僵尸路径。

## 这份文档取代了什么

根目录原来的 `AGENT_INTEGRATION_COMPLETED.md` 描述的是一套**没有发生过的**集成：从
`z-biz-tool-shared` 导入 `AgentPanel` / `QueryHistoryViewer`、`useAgentStore.getState().setContext({...})`、
`query() / optimize() / analyze() / diagnoseError()` 这些方法——代码里一个都不存在
（store 只有 `messages` / `busy` / `handler` 和 `ask`/`setHandler`/`clear`）。它会把人（和 AI）
带到不存在的设计上去，所以删掉了。顺带删掉两个只被那份文档提到的死组件：
`src/agent/QueryHistoryViewer.tsx`（读历史的 `loadHistory()` 是 TODO，永远空表）与
`src/agent/SQLEditor.tsx`（真正在用的是一个编辑器 `src/components/SqlEditor.tsx`，CodeMirror 6）。
`package.json` 里的 `z-biz-tool-shared` 依赖至今 `src/**` 零 import，`App.tsx` 也留着
"临时方案，待共享库修复后迁移"的注释——迁移没做，这里如实记着。
