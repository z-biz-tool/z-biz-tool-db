/**
 * 「这份 AI 配置够不够发请求」—— db 仓前端侧判据。
 *
 * ## 为什么要有这个文件
 *
 * 后端 `report::ai::ensure_configured()`（`src-tauri/src/report/ai.rs`）已经
 * 统一了判定：地址必填，**密钥在私网地址上可空**（那类服务没有鉴权层）。
 *
 * 前端也得判同一件事 —— 否则出现**判据分裂**：后端放行、前端却弹
 * 「先配置 AI 服务地址、密钥与模型」，把指向 mini 网关的用户挡在门外，
 * 而**没有任何编译错误**能暴露它。
 *
 * ## 本仓没有预设概念
 *
 * db 的 AI 配置只有 base_url / api_key / model 三个字段，没有 provider，
 * 后端也没有预设表。所以前端只需要「按地址判」这一条 ——
 * 与后端 `ensure_configured()` 的私网分支同口径。
 *
 * ## 金标
 *
 * Rust 侧 `z-biz-tool-capability/crates/cap-llm/src/config.rs` 的
 * `is_private_endpoint()`。共享层 `z-biz-tool-shared/ai/endpoint` 是它的逐条镜像。
 *
 * 从 `./ai/endpoint` 而不是 `./ai` 导入：`ai` 的 barrel 会拉进 AIManager
 * → zustand，而这里只需要一个纯函数。
 */
import { isPrivateEndpoint } from "z-biz-tool-shared/ai/endpoint";

/** 前端侧的 AI 配置形状（驼峰，`App.tsx` 用）。 */
export interface AiConfigLike {
  baseUrl: string;
  apiKey: string;
  model: string;
}

/** 后端侧的 AI 配置形状（蛇形，`ReportWorkbench.tsx` 用）。 */
export interface AiConfigSnakeLike {
  base_url: string;
  api_key: string;
  model: string;
}

/**
 * 这份配置能不能发请求。
 *
 * 与后端 `ensure_configured()` 同口径：
 * - 地址必填；
 * - 模型必填；
 * - 密钥：私网地址可空（自建服务没有鉴权层），公网必填。
 */
export function isAiConfigured(cfg: AiConfigLike | AiConfigSnakeLike): boolean {
  const camel = cfg as AiConfigLike;
  const snake = cfg as AiConfigSnakeLike;
  const baseUrl = camel.baseUrl ?? snake.base_url ?? "";
  const apiKey = camel.apiKey ?? snake.api_key ?? "";
  const model = camel.model ?? snake.model ?? "";

  if (!baseUrl || !baseUrl.trim()) return false;
  if (!model || !model.trim()) return false;
  if (apiKey && apiKey.trim()) return true;
  return isPrivateEndpoint(baseUrl.trim());
}

/**
 * 这份配置要不要 API Key（界面提示用）。
 *
 * 与 `isAiConfigured()` 同源，避免两个函数给出不同结论。
 */
export function aiNeedsApiKey(cfg: AiConfigLike | AiConfigSnakeLike): boolean {
  return !isAiConfigured(cfg);
}
