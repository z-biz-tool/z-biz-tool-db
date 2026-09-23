// 报表语义层的 IPC 封装。命令注册在 src-tauri/src/lib.rs 的 generate_handler!。
//
// Tauri v2 会把 Rust 侧的 snake_case 形参名映射成 camelCase 的 payload 键，
// 所以只有多词参数（max_repairs → maxRepairs）需要转写；结构体字段仍按
// serde 原样（snake_case）传递。

import { invoke } from "@tauri-apps/api/core";
import type {
  CatalogTable,
  ColumnInfo,
  DatasetPayload,
  DatasetSpec,
  DraftResult,
  PriorDraft,
  PriorReport,
  SavedReport,
  SqlDraft,
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

/** 自然语言 → 一整张报表的规格。
 *  prior 是上一版设计：追问式改稿时把工作台上当前的 spec 带回去，后端会在提示词里
 *  请模型在它基础上改，改出来的设计过的还是同一套本机校验（旧稿里的编造字段照样会被拦下）。 */
export const aiReportDraft = (
  question: string,
  catalog: CatalogTable[],
  config: AIConfig,
  maxRepairs?: number,
  prior?: PriorReport | null
): Promise<DraftResult> =>
  invoke<DraftResult>("ai_report_draft", { question, catalog, config, maxRepairs, prior: prior ?? null });

/** 自然语言 → 一条 SQL。表与列由本机目录限定，编出来的字段会被后端打回。
 *  prior 是上一稿：追问式改稿时带上，后端会在提示词里请模型在旧稿上改，
 *  改出来的稿子过的还是同一套本机校验（旧稿里的编造字段照样会被拦下）。 */
export const aiSqlGenerate = (
  question: string,
  catalog: CatalogTable[],
  config: AIConfig,
  maxRepairs?: number,
  prior?: PriorDraft | null
): Promise<SqlDraft> =>
  invoke<SqlDraft>("ai_sql_generate", { question, catalog, config, maxRepairs, prior: prior ?? null });

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
): Promise<ColumnInfo[]> => invoke<ColumnInfo[]>("report_describe_columns", { config, schema, table });

/** 探查结果 → 目录字段：columns 只放名字（本机校验比对的口径），类型另放一处 */
export const catalogColumns = (cols: ColumnInfo[]) => ({
  columns: cols.map((c) => c.name),
  column_types: cols.reduce(
    (m, c) => {
      if (c.data_type) m[c.name] = c.data_type;
      return m;
    },
    {} as Record<string, string>
  ),
});

/** 给人看的列摘要，与提示词里的口径一致：有类型带类型，没类型只给名字 */
export const columnSummary = (t: CatalogTable) =>
  t.columns
    .map((c) => {
      const ty = t.column_types?.[c];
      return ty ? `${c} ${ty}` : c;
    })
    .join(", ");

export const listTables = (config: BackendConfig): Promise<TableSummary[]> =>
  invoke<TableSummary[]>("get_tables", { config });

// ================== 报表簿 ==================
// 存的是 spec 而不是结果：结果依赖库里当下的数据，spec 才是明天还能重跑的东西。

export const saveReport = (item: SavedReport): Promise<void> =>
  invoke<void>("save_report", { item });

/** 后端按 updated_at 倒序返回，刚改过的排在最前 */
export const loadReports = (): Promise<SavedReport[]> => invoke<SavedReport[]>("load_reports");

export const deleteReport = (id: string): Promise<void> => invoke<void>("delete_report", { id });
