//! 统一 AI 配置的接入胶水（db 仓）。
//!
//! 与 terminal / file / kb 样板同一结构，本仓差异只有两点：
//!
//! 1. **旧配置没有 provider 概念**。db 原先的 `ai_config.json` 只有
//!    `base_url` / `api_key` / `model` 三个字段，服务商靠地址区分。
//!    迁移器已按地址反查预设 id（`cap_llm_config::migrate`），所以本仓
//!    拿到的统一配置里 `provider` 可能是 `custom` —— 语义上最保守，
//!    地址原样保留，用户照样能用。
//!
//! 2. **本仓的 AI 出口有两类**：
//!    - 报表链路（`report/ai.rs`）：提示词带真实列清单，产出还要过
//!      本机校验，属于「模型写、本机审」。
//!    - 通用问答链路（`ai_explain_results` / `ai_optimize_sql` …）：
//!      纯文本问答。
//!    两类都从统一落点取配置，**前端不再回传明文 Key**。
//!
//! ## 命令名对齐（家族基准）
//!
//! | 命令 | 作用 |
//! |---|---|
//! | `get_ai_config` | 读配置（**脱敏视图**：apiKey 恒空串，hasApiKey 表达有无） |
//! | `save_ai_config` | 写配置（apiKey 空串 = 继承已保存的密钥） |
//! | `list_llm_presets` | 供应商预设目录 |
//! | `list_llm_models` | 列模型（草稿覆盖） |
//! | `test_ai_config` | 连通性探测（草稿覆盖） |
//!
//! ## 本改造拔掉的三处毛病
//!
//! - **明文落盘**：`ai_config.json` 里的 api_key 是明文，权限跟 umask 走。
//! - **明文回传前端**：`get_ai_config` 虽然清空了 key，但前端仍把
//!   `api_key` 塞进每个 `ai_*` 命令的参数里 —— 明文在 JS 内存里躺一整轮。
//! - **判据分裂**：前端 `isAiConfigured` 与后端 `ensure_configured`
//!   各判一次，两边漂移过一次（前端要求 model 非空、后端不要求）。

use serde::{Deserialize, Serialize};

use cap_llm_config::{
    migrate_legacy_configs, shared_secret_store, LlmConfigFile, LlmSecret, StoreError,
};

/// 本仓对外的 AI 配置形状（脱敏视图）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConfigView {
    /// 预设 id（cap-llm 的 preset id），空串表示还没配过。
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    /// 恒为空串 —— 明文密钥不出 Rust。
    pub api_key: String,
    /// 是否已保存密钥。
    pub has_api_key: bool,
}

/// 草稿配置（前端设置表单送来的形状）。
///
/// 与 [`AiConfigView`] 的差别只有 `api_key`：草稿里它**非空才生效**，
/// 空串表示「继承已保存的密钥」—— 统一面板不回显密钥，用户改个模型名
/// 再点「测试」是常态，那种情况下密钥框本来就是空的。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConfigDraft {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// 超时（毫秒）。统一落点也是毫秒，与共享面板一致。
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

fn err_msg(e: StoreError) -> String {
    e.to_string()
}

/// 统一落点配置 → cap-llm 配置（+ 独立取出的明文密钥）。
///
/// 谁真正发请求，谁就该从这里拿配置 —— 本仓**任何**发请求的地方都走
/// 这一个函数，不再各自翻译一遍。
///
/// 旧配置的 `provider` 允许 `"openai" | "anthropic" | "ollama" | "custom"`
/// 等历史取值；`cap_llm::ProviderKind::parse_with_preset` 同时接受别名
/// （minimax / deepseek / qwen / kimi / claude / gemini…）。
pub fn to_cap(cfg: &LlmConfigFile, api_key: &str) -> Result<cap_llm::LlmConfig, String> {
    let provider = if cfg.preset.is_empty() {
        cfg.provider.as_str()
    } else {
        cfg.preset.as_str()
    };
    let (kind, preset) =
        cap_llm::ProviderKind::parse_with_preset(provider).map_err(|e| e.to_string())?;
    Ok(cap_llm::LlmConfig {
        kind,
        // 统一落点允许 base_url 为空（用预设默认地址），cap-llm 也是这样处理的
        base_url: cfg.base_url.clone(),
        api_key: api_key.to_string(),
        model: cfg.model.clone(),
        timeout: std::time::Duration::from_millis(cfg.timeout_ms.unwrap_or(60_000).max(1_000)),
        // 0.0 是合法温度，所以不能 `if t > 0.0` 判断，直接透传
        temperature: cfg.temperature,
        max_tokens: cfg.max_tokens,
        preset,
    })
}

/// 读统一配置（脱敏视图）。
pub fn read_shared_config() -> Result<AiConfigView, String> {
    let cfg = cap_llm_config::load_config().map_err(err_msg)?;
    let Some(cfg) = cfg else {
        return Ok(AiConfigView {
            provider: String::new(),
            base_url: String::new(),
            model: String::new(),
            temperature: None,
            max_tokens: None,
            api_key: String::new(),
            has_api_key: false,
        });
    };

    // 密钥在 OS 密钥库里，不在配置文件里 —— 「有没有」要问密钥库。
    let has_api_key = shared_secret_store()
        .get()
        .map_err(err_msg)?
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false);

    Ok(AiConfigView {
        provider: if cfg.preset.is_empty() {
            cfg.provider.clone()
        } else {
            cfg.preset.clone()
        },
        base_url: cfg.base_url.clone(),
        model: cfg.model.clone(),
        temperature: cfg.temperature.map(|t| t as f64),
        max_tokens: cfg.max_tokens,
        api_key: String::new(),
        has_api_key,
    })
}

/// 读统一配置 + 明文密钥（**仅供后端发请求用**，绝不回传前端）。
pub fn read_shared_config_with_key() -> Result<(LlmConfigFile, String), String> {
    let cfg = cap_llm_config::load_config()
        .map_err(err_msg)?
        .ok_or_else(|| "尚未配置 AI 服务商".to_string())?;
    let key = shared_secret_store()
        .get()
        .map_err(err_msg)?
        .unwrap_or_default();
    Ok((cfg, key))
}

/// 写统一配置。
///
/// `api_key` 为空串 ⇒ **继承已保存的密钥**（编辑其它字段时表单里没有明文
/// key，留空是常态而不是「清除」）。
pub fn write_shared_config(draft: &AiConfigDraft) -> Result<(), String> {
    let store = shared_secret_store();

    // 先落配置（不含密钥），再落密钥 —— 配置落盘失败时不会有「有配置没密钥」的半截状态。
    let file = LlmConfigFile {
        preset: draft.provider.clone(),
        provider: draft.provider.clone(),
        base_url: draft.base_url.clone(),
        model: draft.model.clone(),
        temperature: draft.temperature.map(|t| t as f32),
        max_tokens: draft.max_tokens,
        timeout_ms: draft.timeout_ms,
        updated_at_ms: now_ms(),
    };
    cap_llm_config::save_config(&file).map_err(err_msg)?;

    if !draft.api_key.trim().is_empty() {
        store.set(&draft.api_key).map_err(err_msg)?;
    }
    Ok(())
}

/// 迁移一次历史配置（幂等；统一落点已有配置时什么都不做）。
///
/// 本仓旧落点是明文 `data_local_dir()/z-biz-tool-db/ai_config.json`，
/// `cap_llm_config::migrate_legacy_configs` 已按该路径的三平台形态登记。
/// 迁移时明文密钥搬进 OS 密钥库，旧文件里的密钥字段被抹掉。
pub fn migrate_legacy() -> Result<Option<String>, String> {
    let report = migrate_legacy_configs().map_err(err_msg)?;
    Ok(if report.migrated { report.from } else { None })
}

/// 当前时刻（毫秒时间戳）。写失败不影响主流程 —— 时间戳只是诊断辅助。
fn now_ms() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

// ==================== Tauri 命令 ====================

/// 读 AI 配置（脱敏视图：apiKey 恒空串）。
#[tauri::command]
pub fn get_ai_config() -> Result<AiConfigView, String> {
    read_shared_config()
}

/// AI 配置的**就绪状态**（前端唯一的判据来源）。
///
/// ## 为什么不让前端自己判
///
/// 前端原先有一份 `isAiConfigured`（`src/report/aiNeedsKey.ts`），与后端
/// `ensure_configured()` 各判一次 —— 已经漂移过一次（前端要求 model 非空、
/// 后端不要求），而且这次漂移的代价是**静默的**：前端放行、后端拒绝，
/// 用户看到的是一句「请先配置 AI」却不知道自己明明配过。
///
/// 现在判据只有一份，且这一份就是发请求前真正用的那份
/// （`cap_llm::LlmConfig::validate()`）⇒ 前端显示的「能用」与后端的
/// 实际行为**必然一致**，不存在第二套口径。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConfigStatus {
    /// 是否已配置到能发请求。
    pub configured: bool,
    /// 没配好时的一句人话（配好了则为空串）。
    pub reason: String,
}

/// 查 AI 配置是否就绪。
#[tauri::command]
pub fn get_ai_config_status() -> AiConfigStatus {
    match current_cap().and_then(|cfg| {
        cfg.validate().map_err(|e| e.to_string())?;
        Ok(cfg)
    }) {
        Ok(_) => AiConfigStatus {
            configured: true,
            reason: String::new(),
        },
        Err(reason) => AiConfigStatus {
            configured: false,
            reason,
        },
    }
}

/// 保存 AI 配置（密钥进 OS 密钥库，配置落统一目录）。
#[tauri::command]
pub fn save_ai_config(config: AiConfigDraft) -> Result<(), String> {
    write_shared_config(&config)
}

/// 测试 AI 配置是否可用（接受草稿覆盖 —— 用户填完 Key 不用先保存就能测试）。
///
/// 走 `cap_llm::ping`，发一个 `max_tokens=1` 的真实对话：
/// 能拿到回复 ⇒ key、地址、模型三项全对，这才是用户点「测试连接」想知道的。
#[tauri::command]
pub async fn test_ai_config(
    override_config: Option<AiConfigDraft>,
) -> Result<String, cap_llm::LlmFailure> {
    let cap = build_cap_for_draft(override_config).map_err(cap_llm::LlmFailure::from)?;
    let elapsed = cap_llm::ping(&cap)
        .await
        .map_err(cap_llm::LlmFailure::from_error)?;
    Ok(format!(
        "连接成功（{} · {}），耗时 {}ms",
        cap.kind.label(),
        cap.model,
        elapsed.as_millis()
    ))
}

/// 给界面的预设视图。
///
/// **为什么不直接序列化 `ProviderPreset`**：那个结构里的 `protocol` / `kind`
/// 是 Rust 枚举，前端拿到 `"OpenAiCompat"` 这种字符串也没有用武之地；
/// 而 `aliases` / `verified_on` 前端确实要。所以这里只挑前端真正消费的字段。
#[derive(Debug, Serialize)]
pub struct PresetView {
    /// 稳定机器标识，作为 `provider` 字段写回配置。
    pub id: &'static str,
    /// 展示名（中文）。
    pub label: &'static str,
    /// 已填好的服务地址。
    pub base_url: &'static str,
    /// 分组键（international / domestic / aggregator / selfHosted）。
    pub category: &'static str,
    /// 分组展示名。
    pub category_label: &'static str,
    /// 是否需要 API Key。Ollama 这类本地服务不需要，界面要据此隐藏密钥框。
    pub requires_api_key: bool,
    /// 供搜索的其他写法（英文名、旧称）。
    pub aliases: &'static [&'static str],
    /// 地址核对日期。
    pub verified_on: &'static str,
}

fn category_key(c: cap_llm::preset::Category) -> &'static str {
    use cap_llm::preset::Category as C;
    match c {
        C::International => "international",
        C::Domestic => "domestic",
        C::Aggregator => "aggregator",
        C::SelfHosted => "selfHosted",
    }
}

/// 列出可选的供应商预设（单一事实源在 `cap-llm::presets`）。
#[tauri::command]
pub fn list_llm_presets() -> Vec<PresetView> {
    cap_llm::preset::PRESETS
        .iter()
        .map(|p| PresetView {
            id: p.id,
            label: p.label,
            base_url: p.base_url,
            category: category_key(p.category),
            category_label: p.category.label(),
            requires_api_key: p.requires_api_key,
            aliases: p.aliases,
            verified_on: p.verified_on,
        })
        .collect()
}

/// 明显不是对话模型的模型名前缀。
///
/// 用户点「获取模型列表」是为了选一个**能聊天**的模型；而 `/models` 通常把
/// 向量、语音、绘图、重排模型全都列出来。一刀切过滤会把「唯一可用的那个」
/// 也滤掉 —— 排序而非过滤，最坏情况只是用户多滚一点。
const NON_CHAT_HINTS: &[&str] = &[
    "embedding",
    "embed",
    "whisper",
    "tts",
    "audio",
    "image",
    "dall-e",
    "rerank",
    "moderation",
    "guard",
    "search-",
    "-search",
    "ocr",
];

/// 按「能不能聊天」给模型名排序：能聊的在前，其余在后。
fn rank_models(mut models: Vec<String>) -> Vec<String> {
    models.sort_by_key(|m| {
        let lower = m.to_ascii_lowercase();
        let non_chat = NON_CHAT_HINTS.iter().any(|h| lower.contains(h));
        (non_chat, lower.clone())
    });
    models.dedup();
    models
}

/// 列出该配置可用的模型（接受草稿覆盖）。
#[tauri::command]
pub async fn list_llm_models(
    override_config: Option<AiConfigDraft>,
) -> Result<Vec<String>, cap_llm::LlmFailure> {
    let cap = build_cap_for_draft(override_config).map_err(cap_llm::LlmFailure::from)?;
    let models = cap_llm::list_models(&cap)
        .await
        .map_err(cap_llm::LlmFailure::from_error)?;
    Ok(rank_models(models))
}

/// 草稿 → cap 配置；草稿没填的项从统一落点继承。
fn build_cap_for_draft(draft: Option<AiConfigDraft>) -> Result<cap_llm::LlmConfig, String> {
    let draft = draft.unwrap_or_default();
    let has_draft_key = !draft.api_key.trim().is_empty();
    let has_any_draft = has_draft_key
        || !draft.provider.trim().is_empty()
        || !draft.base_url.trim().is_empty()
        || !draft.model.trim().is_empty()
        || draft.temperature.is_some()
        || draft.max_tokens.is_some()
        || draft.timeout_ms.is_some();
    if !has_any_draft {
        // 没有任何草稿 → 完全用已保存的配置
        let (cfg, key) = read_shared_config_with_key()?;
        return to_cap(&cfg, &key);
    }
    let mut file = LlmConfigFile {
        preset: draft.provider.clone(),
        provider: draft.provider.clone(),
        base_url: draft.base_url.clone(),
        model: draft.model.clone(),
        temperature: draft.temperature.map(|t| t as f32),
        max_tokens: draft.max_tokens,
        timeout_ms: draft.timeout_ms,
        updated_at_ms: None,
    };
    // 草稿没填的项从统一落点补齐（继承）
    if let Ok(Some(saved)) = cap_llm_config::load_config() {
        if file.preset.is_empty() && file.provider.is_empty() {
            file.preset = saved.preset.clone();
            file.provider = saved.provider.clone();
        }
        if file.base_url.trim().is_empty() {
            file.base_url = saved.base_url.clone();
        }
        if file.model.trim().is_empty() {
            file.model = saved.model.clone();
        }
        if file.temperature.is_none() {
            file.temperature = saved.temperature;
        }
        if file.max_tokens.is_none() {
            file.max_tokens = saved.max_tokens;
        }
        if file.timeout_ms.is_none() {
            file.timeout_ms = saved.timeout_ms;
        }
    }
    let key = if has_draft_key {
        draft.api_key
    } else {
        // 草稿没填密钥 → 用统一落点里已存的那份
        shared_secret_store()
            .get()
            .map_err(err_msg)?
            .unwrap_or_default()
    };
    to_cap(&file, &key)
}

/// 本仓发请求时用的配置。
///
/// **前端不再回传明文 Key** —— 所有 `ai_*` 命令都从这里取配置。
pub fn current_cap() -> Result<cap_llm::LlmConfig, String> {
    let (cfg, key) = read_shared_config_with_key()?;
    to_cap(&cfg, &key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 环境变量是进程级的，所有碰统一落点的测试串行。
    static LLM_ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_llm_env() -> std::sync::MutexGuard<'static, ()> {
        LLM_ENV_GUARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn get_ai_config_never_returns_the_plaintext_key() {
        let _guard = lock_llm_env();
        let _iso = cap_llm_config::testing::IsolatedLlm::new("db-view");
        write_shared_config(&AiConfigDraft {
            provider: "deepseek".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-chat".into(),
            api_key: "sk-secret-db".into(),
            temperature: Some(0.0),
            max_tokens: None,
            timeout_ms: None,
        })
        .expect("保存应成功");

        let view = read_shared_config().expect("读取应成功");
        assert_eq!(view.api_key, "", "明文密钥绝不能回传前端");
        assert!(view.has_api_key, "有密钥要用布尔表达");
        assert_eq!(view.provider, "deepseek");
        assert_eq!(view.model, "deepseek-chat");
    }

    #[test]
    fn saved_config_file_on_disk_has_no_api_key_field() {
        let _guard = lock_llm_env();
        let _iso = cap_llm_config::testing::IsolatedLlm::new("db-disk");
        write_shared_config(&AiConfigDraft {
            provider: "deepseek".into(),
            base_url: String::new(),
            model: "deepseek-chat".into(),
            api_key: "sk-secret-db-disk".into(),
            temperature: None,
            max_tokens: None,
            timeout_ms: None,
        })
        .expect("保存应成功");

        let path = cap_llm_config::config_path().expect("应能取到路径");
        let content = std::fs::read_to_string(&path).expect("配置文件应存在");
        assert!(
            !content.contains("sk-secret-db-disk"),
            "密钥明文落盘了！配置文件里不该有任何密钥"
        );
        assert!(
            !content.to_lowercase().contains("apikey"),
            "配置文件里不该有 apiKey 字段"
        );
    }

    #[test]
    fn save_with_empty_api_key_keeps_the_saved_key() {
        let _guard = lock_llm_env();
        let _iso = cap_llm_config::testing::IsolatedLlm::new("db-inherit");
        write_shared_config(&AiConfigDraft {
            provider: "deepseek".into(),
            base_url: String::new(),
            model: "deepseek-chat".into(),
            api_key: "sk-first-db".into(),
            temperature: None,
            max_tokens: None,
            timeout_ms: None,
        })
        .expect("首次保存应成功");

        // 改模型名，密钥框留空（面板不回显密钥时的常态）
        write_shared_config(&AiConfigDraft {
            provider: "deepseek".into(),
            base_url: String::new(),
            model: "deepseek-reasoner".into(),
            api_key: String::new(),
            temperature: None,
            max_tokens: None,
            timeout_ms: None,
        })
        .expect("二次保存应成功");

        let (_, key) = read_shared_config_with_key().expect("应能读到配置");
        assert_eq!(key, "sk-first-db", "空串密钥应继承，而不是清除");
    }

    #[test]
    fn draft_overrides_saved_config_for_test_and_models() {
        let _guard = lock_llm_env();
        let _iso = cap_llm_config::testing::IsolatedLlm::new("db-draft");
        write_shared_config(&AiConfigDraft {
            provider: "deepseek".into(),
            base_url: String::new(),
            model: "deepseek-chat".into(),
            api_key: "sk-saved-db".into(),
            temperature: None,
            max_tokens: None,
            timeout_ms: None,
        })
        .expect("保存应成功");

        // 用户在面板里把模型改成别的，还没保存就点测试
        let cap = build_cap_for_draft(Some(AiConfigDraft {
            provider: String::new(),
            base_url: String::new(),
            model: "deepseek-reasoner".into(),
            api_key: String::new(),
            temperature: None,
            max_tokens: None,
            timeout_ms: None,
        }))
        .expect("草稿转换应成功");
        assert_eq!(cap.model, "deepseek-reasoner", "草稿的模型应生效");
        assert_eq!(cap.api_key, "sk-saved-db", "密钥应继承已保存的");
    }

    #[test]
    fn presets_expose_every_field_the_ui_needs() {
        let all = list_llm_presets();
        assert!(all.len() >= 16, "预设不应少于 16 条，实际 {}", all.len());
        for p in &all {
            assert!(!p.base_url.is_empty(), "{} 缺地址", p.id);
            assert!(!p.label.is_empty(), "{} 缺展示名", p.id);
            assert!(!p.category_label.is_empty(), "{} 缺分组名", p.id);
            assert!(!p.verified_on.is_empty(), "{} 缺核对日期", p.id);
        }
        for id in [
            "deepseek",
            "qwen",
            "moonshot",
            "zhipu",
            "volcengine",
            "minimax",
        ] {
            assert!(all.iter().any(|p| p.id == id), "缺国内预设 {}", id);
        }
        let ollama = all.iter().find(|p| p.id == "ollama").unwrap();
        assert!(!ollama.requires_api_key);
    }

    #[test]
    fn every_preset_id_is_writable_back_as_provider() {
        for p in list_llm_presets() {
            let (_kind, preset) = cap_llm::ProviderKind::parse_with_preset(p.id)
                .unwrap_or_else(|e| panic!("预设 {} 无法写回：{}", p.id, e));
            assert_eq!(preset, Some(p.id), "预设 {} 写回后身份丢了", p.id);
        }
    }

    #[test]
    fn chat_models_are_ranked_before_embedding_and_speech() {
        let ranked = rank_models(vec![
            "text-embedding-3-small".into(),
            "gpt-4o".into(),
            "whisper-1".into(),
            "deepseek-chat".into(),
        ]);
        assert_eq!(ranked[0], "deepseek-chat");
        assert_eq!(ranked[1], "gpt-4o");
        assert!(ranked.contains(&"text-embedding-3-small".to_string()));
    }

    /// db 旧配置没有 provider 字段 —— 迁移后 provider 会是 `custom`。
    /// `custom` 必须能被 `to_cap` 接受（否则「配过却说没配」）。
    #[test]
    fn custom_provider_from_legacy_migration_is_usable() {
        let cfg = LlmConfigFile {
            preset: "custom".into(),
            provider: "custom".into(),
            base_url: "https://my-proxy.internal/v1".into(),
            model: "my-model".into(),
            ..Default::default()
        };
        let cap = to_cap(&cfg, "sk-x").expect("custom 必须能转成 cap 配置");
        assert_eq!(cap.base_url, "https://my-proxy.internal/v1");
        assert_eq!(cap.model, "my-model");
    }

    /// 就绪状态：**配好了**就说配好了。
    #[test]
    fn status_reports_ready_when_the_config_is_usable() {
        let _guard = lock_llm_env();
        let _iso = cap_llm_config::testing::IsolatedLlm::new("db-status-ok");
        write_shared_config(&AiConfigDraft {
            provider: "deepseek".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-chat".into(),
            api_key: "sk-status-db".into(),
            temperature: None,
            max_tokens: None,
            timeout_ms: None,
        })
        .expect("保存应成功");

        let st = get_ai_config_status();
        assert!(st.configured, "配好了就该说配好了：{}", st.reason);
        assert_eq!(st.reason, "", "配好了就不该再给理由");
    }

    /// 就绪状态：**没配**时给的是**具体缺哪一项**，不是一句空话。
    ///
    /// 前端此前弹的是写死的一句「先配置 AI 服务地址、密钥与模型」——
    /// 用户明明只差模型名，却被告知要填密钥。现在的理由直接来自
    /// `cap_llm::LlmConfig::validate()`，也就是**发请求前真正跑的那份判据**。
    #[test]
    fn status_explains_what_is_missing() {
        let _guard = lock_llm_env();
        let _iso = cap_llm_config::testing::IsolatedLlm::new("db-status-missing");
        // 只写了地址与密钥，模型名空着
        write_shared_config(&AiConfigDraft {
            provider: "deepseek".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            model: String::new(),
            api_key: "sk-status-db".into(),
            temperature: None,
            max_tokens: None,
            timeout_ms: None,
        })
        .expect("保存应成功");

        let st = get_ai_config_status();
        assert!(!st.configured, "模型名空着就不该说配好了");
        assert!(
            st.reason.contains("模型"),
            "理由要指到模型那一项，实际是：{}",
            st.reason
        );
    }

    /// 私网地址 + 无密钥 ⇒ **配好了**（自建服务没有鉴权层）。
    ///
    /// 这是本仓最早修的那个缺口：此前一律要求密钥，于是指向 mini 网关的
    /// 用户连保存都保存不了。现在判据只有一份，且这一份在后端。
    #[test]
    fn status_is_ready_for_a_keyless_private_endpoint() {
        let _guard = lock_llm_env();
        let _iso = cap_llm_config::testing::IsolatedLlm::new("db-status-private");
        write_shared_config(&AiConfigDraft {
            provider: "custom".into(),
            base_url: "http://192.168.31.249:8200/v1".into(),
            model: "zBiz-Text-35B".into(),
            api_key: String::new(),
            temperature: None,
            max_tokens: None,
            timeout_ms: None,
        })
        .expect("保存应成功");

        let st = get_ai_config_status();
        assert!(st.configured, "私网免密钥的服务应当算配好了：{}", st.reason);
    }

    /// 就绪状态**只由后端判**，前端不再复刻一份 —— 这是本仓的核心判据收敛。
    ///
    /// 阴性对照：把模型名清空，状态必须**从配好变成没配好**。
    /// 若判据被写成恒真，这条立刻红；而恒真的判据会让用户以为配好了，
    /// 点下去只得到一句后端错误 —— 典型的「界面说成功、事实不是」。
    #[test]
    fn status_flips_when_the_config_breaks() {
        let _guard = lock_llm_env();
        let _iso = cap_llm_config::testing::IsolatedLlm::new("db-status-flip");
        write_shared_config(&AiConfigDraft {
            provider: "deepseek".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-chat".into(),
            api_key: "sk-status-db".into(),
            temperature: None,
            max_tokens: None,
            timeout_ms: None,
        })
        .expect("保存应成功");
        assert!(get_ai_config_status().configured);

        // 改坏：模型名清空
        write_shared_config(&AiConfigDraft {
            provider: "deepseek".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            model: String::new(),
            api_key: String::new(),
            temperature: None,
            max_tokens: None,
            timeout_ms: None,
        })
        .expect("二次保存应成功");
        assert!(
            !get_ai_config_status().configured,
            "模型名空了就不该还说配好了"
        );
    }
}
