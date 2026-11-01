// AI 解说链路的 IPC 封装：命令注册在 lib.rs，统一走 report::ai::chat
// （OpenAI 兼容、60 秒超时、错误带状态码）。
//
// 这四条只回文字，不改数据：真正会动库的是生成 SQL 与执行，
// 前者有 ai_sql 的本机校验，后者有 T-006/T-024 的审批门禁。
//
// Tauri v2 把 Rust 侧 snake_case 形参映射成 camelCase 的 payload 键，
// 所以 table_schema → tableSchema、error_message → errorMessage。
//
// ## 为什么这四个函数**不再收 config**
//
// 旧签名带 `config: AIConfig`（含明文 api_key），前端每次调用都把密钥
// 塞进 IPC 载荷 —— 明文因此在 Rust → IPC → JS 内存之间来回搬运。
// 现在配置统一落在 `~/.z-biz-tool/llm/config.json` + OS 密钥库，
// 后端发请求时自己取，前端只负责「有没有」这一个布尔。

import { invoke } from "@tauri-apps/api/core";

export interface ColumnSchema {
  name: string;
  data_type: string;
  nullable: boolean;
  is_primary: boolean;
  default_value: string | null;
}

export const aiOptimizeSql = (
  sql: string,
  tableSchema: ColumnSchema[]
): Promise<string> => invoke<string>("ai_optimize_sql", { sql, tableSchema });

export const aiExplainSql = (sql: string): Promise<string> =>
  invoke<string>("ai_explain_sql", { sql });

export const aiDiagnoseError = (errorMessage: string, sql: string): Promise<string> =>
  invoke<string>("ai_diagnose_error", { errorMessage, sql });

/** 只喂样例行，不把整张结果表搬给模型 */
export const aiExplainResults = (sql: string, results: unknown[]): Promise<string> =>
  invoke<string>("ai_explain_results", { sql, results });
