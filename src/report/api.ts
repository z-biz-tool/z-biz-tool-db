// 报表语义层的 IPC 封装。命令注册在 src-tauri/src/lib.rs 的 generate_handler!。
//
// Tauri v2 会把 Rust 侧的 snake_case 形参名映射成 camelCase 的 payload 键，
// 所以只有多词参数（max_repairs → maxRepairs）需要转写；结构体字段仍按
// serde 原样（snake_case）传递。

import { invoke } from "@tauri-apps/api/core";
import type {
  CatalogTable,
  DatasetPayload,
  DatasetSpec,
  DraftResult,
  SqlPreview,
  ValidationReport,
  ViewPayload,
  ViewSpec,
} from "./types";

/** 后端 DBConfig：报表链路与 execute_query 用同一份凭据契约 */
export interface BackendConfig {
  id: string;
  name: string;
  db_type: string;
  host: string;
  port: number;
  username: string;
  password: string;
  database: string;
}

export interface AIConfig {
  base_url: string;
  api_key: string;
  model: string;
}

export interface TableSummary {
  name: string;
  schema: string | null;
  row_estimate: number | null;
  size_bytes: number | null;
}

/** 数据集 id → (源别名 → 列清单)，回填后可跳过探列直接校验 */
export type SchemaStore = Record<string, Record<string, string[]>>;

export const aiReportDraft = (
  question: string,
  catalog: CatalogTable[],
  config: AIConfig,
  maxRepairs?: number
): Promise<DraftResult> =>
  invoke<DraftResult>("ai_report_draft", { question, catalog, config, maxRepairs });

export const reportDatasetValidate = (
  spec: DatasetSpec,
  configs: BackendConfig[],
  schemas?: Record<string, string[]>
): Promise<ValidationReport> =>
  invoke<ValidationReport>("report_dataset_validate", { spec, configs, schemas });

export const reportDatasetSql = (
  spec: DatasetSpec,
  configs: BackendConfig[]
): Promise<SqlPreview[]> => invoke<SqlPreview[]>("report_dataset_sql", { spec, configs });

export const reportDatasetExecute = (
  spec: DatasetSpec,
  configs: BackendConfig[],
  schemas?: Record<string, string[]>
): Promise<DatasetPayload> =>
  invoke<DatasetPayload>("report_dataset_execute", { spec, configs, schemas });

export const reportViewValidate = (
  view: ViewSpec,
  datasets: DatasetSpec[],
  configs: BackendConfig[],
  schemas?: SchemaStore
): Promise<ValidationReport> =>
  invoke<ValidationReport>("report_view_validate", { view, datasets, configs, schemas });

export const reportViewRender = (
  view: ViewSpec,
  datasets: DatasetSpec[],
  configs: BackendConfig[],
  schemas?: SchemaStore
): Promise<ViewPayload> =>
  invoke<ViewPayload>("report_view_render", { view, datasets, configs, schemas });

export const reportDescribeColumns = (
  config: BackendConfig,
  schema: string,
  table: string
): Promise<string[]> => invoke<string[]>("report_describe_columns", { config, schema, table });

export const listTables = (config: BackendConfig): Promise<TableSummary[]> =>
  invoke<TableSummary[]>("get_tables", { config });
