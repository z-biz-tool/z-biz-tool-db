/**
 * 统一 AI 配置面板的本仓适配层（照抄 terminal / file / kb 样板）。
 *
 * ## 本仓的三个差异
 *
 * 1. **旧配置没有 provider 概念**。db 原先的 `ai_config.json` 只有
 *    `base_url` / `api_key` / `model` 三个字段，服务商靠地址区分。
 *    迁移器已按地址反查预设 id，所以本仓读回来的 `provider` 可能是
 *    `custom` —— 面板能正常显示，语义也最保守（保留原地址）。
 *
 * 2. **判据副本退役**。本仓原先有一份前端 `isAiConfigured`（见
 *    `report/aiNeedsKey.ts`），与后端 `ensure_configured()` 各判一次，
 *    已经漂移过一次（前端要求 model 非空、后端不要求）。统一面板的
 *    `draftIsConfigured` 与 Rust `needs_api_key()` 同源，那个分裂在这里消失。
 *
 * 3. **明文密钥不再在 JS 内存里常驻**。旧的 `get_ai_config` 虽然清空了
 *    key，但前端仍把 `api_key` 塞进每个 `ai_*` 命令的参数里 —— 明文在
 *    Rust → IPC → JS 内存 → 表单 DOM 全链路可见。现在 `ai_*` 命令一律
 *    从统一落点取配置，前端只负责「有没有」。
 *
 * ## ⚠️ 键名必须是 camelCase（不是 snake_case）
 *
 * 这是实测出来的、**不是文档抄的**：Tauri v2 只对**顶层命令参数**做
 * snake_case → camelCase 映射；**嵌套结构体走 serde**，键名以结构体上
 * 的 `#[serde(rename_all = ...)]` 为准。后端 `AiConfigDraft` 标的是
 * `camelCase`，所以这里必须送 `baseUrl` / `apiKey` / `timeoutMs`。
 *
 * 送 snake_case 的后果**不是报错**，而是 serde 静默忽略 ⇒
 * `base_url` 变空串、`api_key` 变空串 ⇒ 用户填了密钥却存不进去，
 * 而且没有任何报错能暴露它。kb / file 两仓此前就踩在这上面。
 */

import { invoke } from "@tauri-apps/api/core";
import {
  AiSettingsPanel,
  type AiConfigDraft,
  type AiSettingsActions,
  type AiSettingsPanelProps,
  type LlmFailure,
  type PresetView,
} from "z-biz-tool-shared/ai/settings";

/**
 * 本仓后端的草稿形状（`llm_shared.rs::AiConfigDraft`）。
 *
 * 字段名是 **camelCase** —— 与后端 `#[serde(rename_all = "camelCase")]`
 * 逐字对应。写 snake_case 会被 serde 静默丢弃（见文件头注释）。
 */
interface DbAiConfigDraft {
  provider: string;
  baseUrl: string;
  model: string;
  apiKey: string;
  temperature: number | null;
  maxTokens: number | null;
  timeoutMs: number | null;
}

function draftToConfig(draft: AiConfigDraft): DbAiConfigDraft {
  return {
    provider: draft.preset,
    baseUrl: draft.baseUrl,
    model: draft.model,
    apiKey: draft.apiKey,
    temperature: draft.temperature ?? null,
    maxTokens: draft.maxTokens ?? null,
    timeoutMs: draft.timeoutMs ?? null,
  };
}

/** 后端失败 → 共享面板的失败结构。字段名一致，直接透传。 */
function toPanelFailure(raw: unknown): LlmFailure {
  const f = (raw ?? {}) as Partial<LlmFailure> & {
    error?: string;
    message?: string;
  };
  return {
    kind: f.kind ?? "unknown",
    status: f.status ?? null,
    message: f.message ?? f.error ?? "未知错误",
    hint: f.hint ?? "",
    focus: (f.focus as LlmFailure["focus"]) ?? "none",
  };
}

export function createAiSettingsActions(): AiSettingsActions {
  return {
    async listPresets(): Promise<PresetView[]> {
      const list = await invoke<PresetView[]>("list_llm_presets");
      return Array.isArray(list) ? list : [];
    },

    async listModels(draft: AiConfigDraft): Promise<string[]> {
      try {
        const models = await invoke<string[]>("list_llm_models", {
          overrideConfig: draftToConfig(draft),
        });
        return Array.isArray(models) ? models : [];
      } catch (e) {
        throw toPanelFailure(e);
      }
    },

    async testConnection(draft: AiConfigDraft) {
      try {
        const detail = await invoke<string>("test_ai_config", {
          overrideConfig: draftToConfig(draft),
        });
        return { ok: true, detail };
      } catch (e) {
        return { ok: false, failure: toPanelFailure(e) };
      }
    },

    async save(draft: AiConfigDraft): Promise<boolean> {
      await invoke("save_ai_config", { config: draftToConfig(draft) });
      return true;
    },
  };
}

/** 后端脱敏视图（`llm_shared.rs::AiConfigView`）。 */
export interface DbAiConfigView {
  provider: string;
  baseUrl: string;
  model: string;
  temperature: number | null;
  maxTokens: number | null;
  /** 恒为空串。 */
  apiKey: string;
  hasApiKey: boolean;
}

export function loadAiConfigView(): Promise<DbAiConfigView> {
  return invoke<DbAiConfigView>("get_ai_config");
}

/** 本仓的 AI 配置面板（直接复用共享面板）。 */
export function DbAiSettingsPanel({
  view,
  onSaved,
}: {
  view: DbAiConfigView | null;
  onSaved?: () => void;
}) {
  const props: AiSettingsPanelProps = {
    value: view
      ? {
          preset: view.provider,
          baseUrl: view.baseUrl,
          model: view.model,
          temperature: view.temperature ?? undefined,
          maxTokens: view.maxTokens ?? undefined,
        }
      : undefined,
    hasApiKey: view?.hasApiKey === true,
    actions: createAiSettingsActions(),
    onSaved,
  };
  return <AiSettingsPanel {...props} />;
}
