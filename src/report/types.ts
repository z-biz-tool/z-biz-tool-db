// 报表语义层的前端类型：与 src-tauri/src/report/{dataset,view,source,ai}.rs 的
// serde 结构一一对应。后端没有做 camelCase 重命名，所以这里保持 snake_case，
// 前端拿到的就是线上形状，不做任何"猜字段"。

export type Scalar = string | number | boolean | null;

/** 数据集聚合函数（后端 AggFunc；反序列化大小写不敏感） */
export type AggFuncName = "Count" | "CountDistinct" | "Sum" | "Avg" | "Min" | "Max";
export type JoinKindName = "Inner" | "Left";
/** 组件图表类型（后端 ChartType，序列化为大写） */
export type ChartKind = "LINE" | "BAR" | "PIE" | "TABLE" | "KPI";
/** 组件级聚合（后端 AggType）；RAW = 不聚合，按数据集原样画 */
export type WidgetAgg = "RAW" | "SUM" | "COUNT" | "COUNT_DISTINCT" | "AVG" | "MIN" | "MAX";

export interface SourceRef {
  alias: string;
  connection_id: string;
  /** 可省略：报表链路会按连接真实方言回填 */
  database_type?: string;
  schema?: string;
  table: string;
  /** 空 = SELECT * */
  columns?: string[];
}

export interface JoinPair {
  left: string;
  right: string;
}

export interface JoinSpec {
  source: string;
  on: JoinPair[];
  kind?: JoinKindName;
}

export interface ComputedSpec {
  name: string;
  expr: string;
}

export interface AggSpec {
  output: string;
  func: AggFuncName;
  /** 省略表示 COUNT(*) */
  column?: string | null;
}

export interface SortDir {
  column: string;
  desc?: boolean;
}

export interface DatasetSpec {
  id: string;
  name: string;
  /** 基表别名，必须是 sources 之一 */
  base: string;
  sources: SourceRef[];
  joins?: JoinSpec[];
  filters?: string[];
  computed?: ComputedSpec[];
  group_by?: string[];
  aggregates?: AggSpec[];
  post_computed?: ComputedSpec[];
  fields?: string[];
  sort?: SortDir[];
  limit?: number | null;
  /** 单个源的取数硬闸；撞闸会标 partial */
  max_rows?: number | null;
}

export interface WidgetEncode {
  x?: string | null;
  y?: string | null;
  series?: string | null;
  category?: string | null;
  value?: string | null;
  columns?: string[];
}

export interface WidgetSpec {
  id: string;
  type: ChartKind;
  title?: string;
  dataset: string;
  encode?: WidgetEncode;
  agg?: WidgetAgg;
  filters?: string[];
  limit?: number | null;
}

export interface WidgetLayout {
  widget: string;
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface ViewSpec {
  id: string;
  name: string;
  version?: number;
  widgets: WidgetSpec[];
  /** 留空则自动纵向堆叠 */
  layout?: WidgetLayout[];
}

export interface SqlPreview {
  dataset: string;
  alias: string;
  connection_id: string;
  connection_name: string;
  database_type: string;
  sql: string;
}

export interface ValidationReport {
  steps: string[];
  sqls: SqlPreview[];
  /** 数据集 id → 输出列 */
  schemas: Record<string, string[]>;
}

export interface SourceRowStat {
  alias: string;
  rows: number;
}

export interface DatasetPayload {
  columns: string[];
  rows: Scalar[][];
  row_count: number;
  source_rows: SourceRowStat[];
  /** 撞上 max_rows 的源别名 */
  truncated: string[];
  partial: boolean;
  generated_sql: SqlPreview[];
  steps: string[];
  elapsed_ms: number;
}

export interface DatasetRunStat {
  id: string;
  name: string;
  rows: number;
  columns: string[];
  truncated: string[];
  partial: boolean;
  elapsed_ms: number;
}

export interface Series {
  name: string;
  values: Scalar[];
}

export interface ChartData {
  widget: string;
  kind: ChartKind;
  title: string;
  categories: string[];
  series: Series[];
  columns: string[];
  rows: Scalar[][];
  /** KPI 的单个标量 */
  value: Scalar;
  row_count: number;
  warnings: string[];
}

/** 没取到数的数据集：整张报表照常渲染，但挂在它上面的组件会被摘掉 */
export interface DatasetFailure {
  id: string;
  name: string;
  /** 后端原话：连不上、表读不出列、引擎拒绝 SQL */
  error: string;
  /** 因为这张集没数而没画出来的组件 id */
  widgets: string[];
}

export interface ViewPayload {
  steps: string[];
  layout: WidgetLayout[];
  charts: ChartData[];
  datasets: DatasetRunStat[];
  generated_sql: SqlPreview[];
  partial: boolean;
  failed: DatasetFailure[];
  elapsed_ms: number;
}

/** 后端探查一列的结果：名字给本机校验用，类型只喂给模型 */
export interface ColumnInfo {
  name: string;
  /** 数据库自报类型，如 `decimal(12,2)`；探不到就是空串 */
  data_type: string;
}

/** 喂给模型的目录：只有结构信息，没有任何连接凭据 */
export interface CatalogTable {
  connection_id: string;
  connection_name?: string;
  database_type: string;
  schema?: string;
  table: string;
  columns: string[];
  /** 列名 → 数据库类型。只进提示词，本机校验仍只比对 columns */
  column_types?: Record<string, string>;
}

export interface ReportDraft {
  datasets: DatasetSpec[];
  view: ViewSpec;
}

export interface DraftResult {
  datasets: DatasetSpec[];
  view: ViewSpec;
  steps: string[];
  columns: Record<string, string[]>;
  warnings: string[];
  /** 模型被本机校验打回了几次 */
  repairs: number;
}

/** ai_sql_generate 的返回：一条过了本机校验的 SQL，以及它是凭什么被校验的 */
export interface SqlDraft {
  sql: string;
  /** 这条 SQL 命中的目录内表，UI 用它说明"依据是什么" */
  tables: string[];
  dialect: string;
  warnings: string[];
  repairs: number;
}

/** 上一稿：追问（"再按月拆开"）时带回去，让模型在已有语句上改而不是从零重写。
 *  question 是写这一稿时那句需求——模型得知道旧稿是为了什么写的。 */
export interface PriorDraft {
  question: string;
  sql: string;
}

/** ai_sql_generate 被本机挡下时的回执（后端 SqlReject）：错误原文 + 被挡下的那条 SQL。
 *  sql 为空 = 还没见到模型写的 SQL 就被拒（请求发不出去、回复里根本没有 SQL），
 *  这时前端不该挂「照这条错误改」——没有底稿可改。 */
export interface SqlReject {
  error: string;
  sql?: string | null;
}

/** 上一版报表设计：追问（"再加一条按月的折线"）时把工作台上当前的 spec 带回去。
 *  手改过的 JSON、从报表簿打开的历史报表都能当上一版，所以 question 可能为空。 */
export interface PriorReport {
  question: string;
  draft: ReportDraft;
}

/** ai_report_draft 被本机挡下时的回执（后端 DraftReject）。
 *  draft 是被挡下的那一稿：模型是单发的，下一轮看不见自己上一轮写了什么，
 *  只回错误原文的话，"组件 w1 绑定的数据集 d1 不存在"里的 d1 它根本对不上。
 *  模型回复连 JSON 都解不开、或请求没发出去时没有底稿可带。 */
export interface DraftReject {
  error: string;
  draft?: ReportDraft | null;
}

/** 报表簿条目：存的是 spec 而不是结果，落盘在后端 queries.rs */
export interface SavedReport {
  id: string;
  name: string;
  description?: string | null;
  /** 起草时的自然语言问题，重新起草时回填 */
  question: string;
  datasets: DatasetSpec[];
  view: ViewSpec;
  created_at: number;
  updated_at: number;
}
