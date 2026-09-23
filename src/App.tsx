import { useState, useEffect, useRef, useMemo, useCallback } from "react";
import SqlEditor from "./components/SqlEditor";
import {
  ConfigProvider,
  theme,
  Layout,
  Menu,
  Button,
  Space,
  Tabs,
  Card,
  Table,
  Input,
  InputNumber,
  Select,
  Form,
  Modal,
  message,
  Tooltip,
  Tag,
  Empty,
  Switch,
  Typography,
  Alert,
  Spin,
  Badge,
  Segmented,
} from "antd";
import { buildGrant, isLikelyWriteSql, PendingApproval } from "./ipc/approval";
import type { ApprovalGrant } from "./ipc/approval";
import { classifyError, errorDescription } from "./ipc/errors";

interface ExportedConnection {
  name: string;
  db_type: string;
  host: string;
  port: number;
  username: string;
  database: string;
}
import {
  DatabaseOutlined,
  PlusOutlined,
  PlayCircleOutlined,
  FormatPainterOutlined,
  CopyOutlined,
  DeleteOutlined,
  EditOutlined,
  SaveOutlined,
  SyncOutlined,
  TableOutlined,
  HistoryOutlined,
  SettingOutlined,
  DownloadOutlined,
  UploadOutlined,
  StarOutlined,
  StarFilled,
  ApiOutlined,
  RobotOutlined,
  CodeOutlined,
  BugOutlined,
  BulbOutlined,
  SnippetsOutlined,
  SearchOutlined,
  DashboardOutlined,
  CheckCircleOutlined,
  ThunderboltOutlined,
} from "@ant-design/icons";
import { invoke } from "@tauri-apps/api/core";
// 使用本地 Agent 组件（临时方案，待共享库修复后迁移到 z-biz-tool-shared）
import { useAgentStore } from "./agent/AgentManager";
import AgentPanel from "./agent/AgentPanel";
import { ReportWorkbench } from "./report/ReportWorkbench";
import { aiSqlGenerate, catalogColumns, listTables, reportDescribeColumns } from "./report/api";
import { aiDiagnoseError, aiExplainResults, aiExplainSql, aiOptimizeSql } from "./ipc/ai";
import type { BackendConfig, TableSummary } from "./report/api";
import type { CatalogTable, ChainOutcome, SqlDraft, SqlReject } from "./report/types";
import type { AgentAskContext, AgentIntent, AgentResponse } from "./agent/types";

// 渐变色主题常量
const brandGradient = "linear-gradient(135deg, #667eea 0%, #764ba2 100%)";
const cardBgGradient =
  "linear-gradient(135deg, rgba(102,126,234,0.04) 0%, rgba(118,75,162,0.04) 100%)";

const { Header, Sider, Content } = Layout;
const { TextArea } = Input;
const { Text } = Typography;

// Rust 风格可空类型
type Option<T> = T | null;

/** 本机挡下这条 SQL 之后，回喂给模型所需的工单：拒因 + 被挡下的那一条 + 当时那句需求。
 *  缺 SQL 那一半就等于让模型对着"orders.net_amount 不存在"凭空猜自己刚写了什么。 */
type SqlFix = { question: string; error: string; sql: string };

/** Tauri v2 会把 Rust 侧的 `Err(SqlReject)` 原样抛成对象，`String(e)` 只会得到
 *  [object Object]；但前置门槛那类仍是纯文本，两种形状都得能拆开。 */
const asSqlReject = (e: unknown): SqlReject => {
  if (e && typeof e === "object" && typeof (e as SqlReject).error === "string") return e as SqlReject;
  return { error: String(e), sql: null };
};

/** 从本机拒因里挑出"不认得的表"。句式来自 ai_sql.rs 的 check_sql
 *  （`SQL 里的表 A、B 不在本次目录里`，多个名字用、相连，与 ai_sql.rs 的同名测试一起改）。
 *  句式变了这里只会抽不到名字、少一句提示——不会拿别的文字去认表，所以不做模糊兜底。 */
const unknownTablesOfVerdict = (verdict: string): string[] => {
  const m = /SQL 里的表 (.+?) 不在本次目录里/.exec(verdict);
  if (!m) return [];
  return m[1]
    .split("、")
    .map((s) => s.trim())
    .filter(Boolean);
};

// 类型定义
interface DBConnection {
  id: string;
  name: string;
  type: "mysql" | "postgresql" | "sqlite" | "sqlserver";
  host: string;
  port: number;
  username: string;
  password: string;
  database: string;
}

// 后端 v2 列元数据（DB-01 / T-001）：前端不再用 Object.keys 推导列名
interface ColumnMeta {
  ordinal: number;
  name: string;
  native_type: string;
  logical_type: string;
  nullable: boolean;
}

// 后端 v2 结果模型：单元格为 tagged { __kind, value }
interface TaggedCell {
  __kind:
    | "null"
    | "integer"
    | "float"
    | "decimal"
    | "text"
    | "binary"
    | "date"
    | "time"
    | "datetime"
    | "timestamp"
    | "uuid"
    | "json"
    | "unsupported"
    | "decode_error";
  value: any;
}

interface QueryResultV2 {
  id: string;
  columns: string[];
  column_meta?: ColumnMeta[];
  rows: Array<TaggedCell[]>;
  affected_rows: number;
  execution_time_ms: number;
  is_query: boolean;
  /** T-031：界面只送前 N 行时，后端把总行数和"确实截断了"一起带回来 */
  truncated?: boolean;
  total_rows?: number;
  timings?: {
    connect_ms: number;
    queue_ms: number;
    execute_ms: number;
    fetch_ms: number;
    total_ms: number;
  };
}

/** 单元格的展示文本（DB-02 / T-003） */
function cellDisplay(cell: TaggedCell | null | undefined): string {
  if (!cell || cell.__kind === "null") return "NULL";
  switch (cell.__kind) {
    case "binary":
      return `二进制 ${(cell.value || "").length ?? 0}B`;
    case "decode_error":
      return `⚠ ${cell.value}`;
    case "unsupported":
      return `？${cell.value}`;
    default:
      return cell.value === null || cell.value === undefined ? "∅" : String(cell.value);
  }
}

interface ColumnInfo {
  name: string;
  data_type: string;
  nullable: boolean;
  is_primary: boolean;
  default_value: Option<string>;
}

interface TableInfo {
  name: string;
  schema: Option<string>;
  row_estimate: Option<number>;
  size_bytes: Option<number>;
}

interface QueryHistoryItem {
  id: string;
  sql: string;
  connection_id: string;
  connection_name: string;
  timestamp: number;
  execution_time_ms: number;
  success: boolean;
  error: Option<string>;
}

interface SavedQuery {
  id: string;
  name: string;
  sql: string;
  description: Option<string>;
  tags: string[];
  created_at: number;
  updated_at: number;
}

function App() {
  const [darkMode, setDarkMode] = useState(false);
  const [connections, setConnections] = useState<DBConnection[]>([]);
  const [selectedConnection, setSelectedConnection] = useState<DBConnection | null>(null);
  const [sqlCode, setSqlCode] = useState("SELECT * FROM users WHERE id = 1");
  const [queryResults, setQueryResults] = useState<TaggedCell[][]>([]);
  // T-031：界面行数上限。0 = 不限。后端是在把整批行取回之后才截断的，
  // 这一闸挡的是 IPC 与表格 DOM（几十万行会把界面压死），库侧该加的还是 LIMIT。
  const [rowCap, setRowCap] = useState<number>(5000);
  const [resultMeta, setResultMeta] = useState<{ total: number; shown: number } | null>(null);
  // T-001 + T-002：以后端 column_meta 为准，零行结果仍有列定义
  const [queryColumnsMeta, setQueryColumnsMeta] = useState<ColumnMeta[]>([]);
  // T-044: 网格编辑状态
  const [editingCell, setEditingCell] = useState<{ rowIndex: number; colIndex: number } | null>(
    null
  );
  const [editValue, setEditValue] = useState<string>("");
  const [changeSet, setChangeSet] = useState<Map<string, TaggedCell>>(new Map());
  // T-044: 记录变更数量用于UI显示
  // T-006：可写访问模式开关；S0 默认 false，写入命令将被后端拒绝
  const [writeAccessEnabled, setWriteAccessEnabled] = useState(false);
  const [isConnected, setIsConnected] = useState(false);
  const [showConnectionModal, setShowConnectionModal] = useState(false);
  const [editingConnection, setEditingConnection] = useState<DBConnection | null>(null);
  const [showImportModal, setShowImportModal] = useState(false);
  const [importPreview, setImportPreview] = useState<ExportedConnection[]>([]);
  const [importError, setImportError] = useState<string>("");
  const [form] = Form.useForm();

  // 多标签页
  const [tabs, setTabs] = useState<{ id: string; title: string; sql: string }[]>([
    { id: "tab-1", title: "查询 1", sql: "SELECT * FROM users WHERE id = 1" },
  ]);
  const [activeTabId, setActiveTabId] = useState("tab-1");

  // 表结构
  const [tables, setTables] = useState<TableInfo[]>([]);
  const [columns, setColumns] = useState<ColumnInfo[]>([]);
  const [selectedTable, setSelectedTable] = useState<string | null>(null);

  // 查询历史 —— T-017 接通后端持久化
  const [history, setHistory] = useState<QueryHistoryItem[]>([]);
  const [historyLoaded, setHistoryLoaded] = useState(false);
  const [historySearch, setHistorySearch] = useState("");

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const list = await invoke<QueryHistoryItem[]>("load_query_history");
        if (!cancelled) {
          setHistory(Array.isArray(list) ? list : []);
          setHistoryLoaded(true);
        }
      } catch (e) {
        if (!cancelled) {
          msgApi.warning(`历史加载失败：${e}（应用不会保存空历史覆盖损坏文件）`);
          setHistoryLoaded(true);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // 历史变更后去抖持久化（500ms）
  useEffect(() => {
    if (!historyLoaded) return;
    const t = setTimeout(() => {
      invoke("save_query_history", { history }).catch((e) => msgApi.error(`保存历史失败：${e}`));
    }, 500);
    return () => clearTimeout(t);
  }, [history, historyLoaded]);

  // 保存的查询 —— T-017 接通后端持久化
  const [savedQueries, setSavedQueries] = useState<SavedQuery[]>([]);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const list = await invoke<SavedQuery[]>("load_saved_queries");
        if (!cancelled) {
          setSavedQueries(Array.isArray(list) ? list : []);
        }
      } catch (e) {
        if (!cancelled) {
          msgApi.warning(`收藏加载失败：${e}`);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // 新增/修改/删除仍走原 handleSave/handleDelete 调用 save_query/delete_saved_query，
  // 这里不再做整体快照保存（避免与单条命令竞态）。
  const [showSaveQueryModal, setShowSaveQueryModal] = useState(false);
  const [editingSavedQuery, setEditingSavedQuery] = useState<SavedQuery | null>(null);

  // T-036: 导出结果为 CSV
  const handleExportResults = () => {
    if (queryResults.length === 0) {
      msgApi.warning("没有查询结果可导出");
      return;
    }
    try {
      const headers = resultColumns.map((c) => c.title);
      const csvRows = queryResults.map((row) => {
        return row.map((cell) => {
          if (cell.__kind === "null") return "NULL";
          if (cell.__kind === "binary") return "[BINARY]";
          const val = String(cell.value ?? "");
          // CSV 公式防护：字段以 = + - @ \t \n 开头时加前缀单引号
          if (/^[=+\-@\t\n]/.test(val)) return "'" + val;
          // 含逗号或引号时用双引号包裹
          if (val.includes(",") || val.includes('"')) {
            return '"' + val.replace(/"/g, '""') + '"';
          }
          return val;
        });
      });
      const csv = [headers.join(","), ...csvRows.map((r) => r.join(","))].join("\n");
      const blob = new Blob([csv], { type: "text/csv;charset=utf-8;" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `query_results_${new Date().toISOString().slice(0, 10)}.csv`;
      a.click();
      URL.revokeObjectURL(url);
      // 截断过就必须说清导的是"画出来的那一段"，不是整批结果——
      // 否则用户拿这份 CSV 去对账，少的行没人认领
      msgApi.success(
        resultMeta
          ? `已导出 ${queryResults.length} 行到 CSV（界面按上限只取回了 ${resultMeta.shown} 行${
              resultMeta.total ? `，这次结果共 ${resultMeta.total} 行` : "，后面的行没取"
            }）`
          : `已导出 ${queryResults.length} 行到 CSV`
      );
    } catch (e: any) {
      msgApi.error(`导出失败: ${e}`);
    }
  };

  // AI 配置
  const [aiConfig, setAiConfig] = useState({
    baseUrl: "",
    apiKey: "",
    model: "xop3qwencodernext",
  });
  const [showAiConfigModal, setShowAiConfigModal] = useState(false);
  const [aiForm] = Form.useForm();

  // 工作模式：SQL 查询 / AI 报表工作台
  const [mode, setMode] = useState<"sql" | "report">("sql");
  // 报表工作台去过一次就留着：以前 mode 一换回 SQL 查询，整块被条件渲染拆掉，
  // 刚挑好的表、起草的草稿、规格 JSON 全跟着没了，用户只能从头再来。
  // 也不是一开始就挂——没去过就别白刷一遍各连接的表清单。
  const [reportSeen, setReportSeen] = useState(false);
  // 从 SQL 那条腿撞进跨库死路时，那句需求要跟着人一起过去（seq 变一次算一次交接）
  const [reportSeed, setReportSeed] = useState<
    { q: string; seq: number; run?: boolean; done?: (o: ChainOutcome) => void } | null
  >(null);
  const reportSeedSeq = useRef(0);

  // T-045 写入审批状态
  const [pendingApproval, setPendingApproval] = useState<PendingApproval | null>(null);
  const [pendingGrant, setPendingGrant] = useState<ApprovalGrant | null>(null);
  const approvalPendingRef = useRef<((g: ApprovalGrant | null) => void) | null>(null);

  const requestApproval = (sql: string, environment: string): Promise<ApprovalGrant | null> =>
    new Promise((resolve) => {
      const ttlSec = 60;
      const pending: PendingApproval = {
        sql,
        environment,
        issuedAt: Math.floor(Date.now() / 1000),
        ttlSec,
      };
      setPendingApproval(pending);
      setPendingGrant(null);
      approvalPendingRef.current = resolve;
    });

  // AI 功能状态
  const [aiLoading, setAiLoading] = useState(false);
  const [aiResult, setAiResult] = useState<string>("");
  const [showAiResultModal, setShowAiResultModal] = useState(false);
  const [aiActiveTab, setAiActiveTab] = useState<string>("generate");
  const [aiNaturalLanguage, setAiNaturalLanguage] = useState("");
  const [aiSqlForOptimize, setAiSqlForOptimize] = useState("");
  const [aiSqlForExplain, setAiSqlForExplain] = useState("");
  const [aiErrorMessage, setAiErrorMessage] = useState("");
  const [aiSqlForError, setAiSqlForError] = useState("");
  // 生成 SQL 这条链的产出：SQL 之外还要把"依据哪几张表、被打回几次"露出来
  const [aiGenTables, setAiGenTables] = useState<string[]>([]);
  const [aiDraft, setAiDraft] = useState<SqlDraft | null>(null);
  // 生成 SQL 这条链的失败现场：标题按"卡在哪一步"分开写，正文是拒因原文（多行按行铺开）。
  // fix 有值 = 本机挡下了一稿并把它带了回来，错误卡上才有「让 AI 照这条错误改」可点。
  const [aiDraftError, setAiDraftError] = useState<
    { title: string; detail: string; fix?: SqlFix } | null
  >(null);
  // 四条解说链路的失败原因，显示在弹窗里而不是只闪一句 toast
  const [aiTextError, setAiTextError] = useState("");
  // 挡下的一稿点名的表如果其实在别的连接里，这一腿怎么改都补不上——一条 SQL 只能进一个库。
  // 这种时候该给用户的是另一条腿的入口，不是"再改一轮"。missing 是问遍了也没找到的那些，
  // failed 是没问到的那些连接（问不到就不能断言"别的库里也没有"）。
  const [crossDb, setCrossDb] = useState<{
    found: { table: string; connection: string }[];
    missing: string[];
    failed: string[];
  } | null>(null);

  const [msgApi, msgContext] = message.useMessage();

  // 执行查询
  // T-009：invoke 前快照 connectionId+activeTabId，响应后比对当前状态
  // T-045：writable + isLikelyWriteSql 时弹审批 Modal 收集 grant
  const executeQuery = async () => {
    if (!selectedConnection) {
      msgApi.warning("请先连接数据库");
      return;
    }
    const requestConnId = selectedConnection.id;
    const requestTabId = activeTabId;
    const wantWrite = writeAccessEnabled;
    const likelyWrite = isLikelyWriteSql(sqlCode);
    let grant: ApprovalGrant | null | undefined = undefined;
    if (wantWrite && likelyWrite) {
      // 弹 Modal 等用户确认；环境暂取 selectedConnection.environment ?? "unknown"
      grant = await requestApproval(sqlCode, (selectedConnection as any).environment ?? "unknown");
      if (!grant) {
        // 用户取消
        msgApi.info("写入操作已取消");
        return;
      }
    }
    try {
      const result = await invoke<QueryResultV2>("execute_query", {
        sql: sqlCode,
        config: toBackendConfig(selectedConnection),
        access_mode: wantWrite ? "writable" : "readOnly",
        approval: grant ?? null,
        expected_generation: null,
        max_rows: rowCap > 0 ? rowCap : null,
      });

      // 异步校验：标签或连接已切走 → 丢弃结果
      if (requestConnId !== selectedConnection?.id || requestTabId !== activeTabId) {
        msgApi.warning("连接或标签已切换，结果已丢弃（DB-11 防竞态）");
        return;
      }

      // 同步覆写列源：以后端 column_meta 为准；零行也有列（A01）
      setQueryColumnsMeta(result.column_meta ?? []);
      setQueryResults(result.rows || []);
      const shown = result.rows?.length || 0;
      setResultMeta(result.truncated ? { total: Number(result.total_rows || 0), shown } : null);

      // 执行记录落进 Agent 会话，但标成 system：那是刚发生过的事，不是用户打的一句话
      useAgentStore.getState().addMessage({
        id: `${Date.now().toString(36)}-run`,
        role: "system",
        // sqlite 走流式时总数没数过（total_rows = 0），这时只能说"后面还有"，
        // 编一个总数出来就是拿界面的数当库里的数
        content: result.truncated
          ? `已执行一条语句，结果超过 ${result.rows?.length || 0} 行，界面按上限只取回前 ${
              result.rows?.length || 0
            } 行（后面的没取），耗时 ${result.execution_time_ms || 0}ms`
          : `已执行一条语句，返回 ${result.rows?.length || 0} 行，耗时 ${
              result.execution_time_ms || 0
            }ms`,
        timestamp: Date.now(),
      });

      // 添加历史
      const newItem: QueryHistoryItem = {
        id: Date.now().toString(),
        sql: sqlCode,
        connection_id: selectedConnection?.id || "",
        connection_name: selectedConnection?.name || "",
        timestamp: Math.floor(Date.now() / 1000),
        execution_time_ms: result.execution_time_ms || 0,
        success: true,
        error: null,
      };
      setHistory((prev) => [newItem, ...prev].slice(0, 100));
      const timing = `总耗时 ${result.execution_time_ms}ms${
        result.timings && result.timings.total_ms > 0
          ? `（连接 ${result.timings.connect_ms}ms / 执行 ${result.timings.execute_ms}ms）`
          : ""
      }`;
      // 截断不能只报绿：这一屏看不到剩下的行，用户以为手里就是全量
      if (result.truncated) {
        msgApi.warning({
          content: `查询执行成功，但结果不止 ${shown} 行，界面按上限只画前 ${shown} 行（后面的没取）——${timing}。要全量请调高上限或在语句里自己收口`,
          duration: 6,
        });
      } else {
        msgApi.success({ content: `查询执行成功，${timing}`, duration: 3 });
      }
    } catch (e: any) {
      if (requestConnId !== selectedConnection?.id || requestTabId !== activeTabId) {
        return;
      }
      msgApi.error({
        content: `${errorDescription(classifyError(String(e)))}`,
        duration: 5,
      });
      const err = classifyError(String(e));
      // 报错原文只闪在 toast 里，五秒就没了；连着当时那条语句一起落进会话，
      // 才能一键交给模型诊断，不用用户凭记忆手抄（T-075）。
      // sqlCode 取的是这次请求闭包里的那份：中途改编辑器也不会记错现场。
      useAgentStore.getState().addMessage({
        id: `${Date.now().toString(36)}-fail`,
        role: "system",
        content: "这条语句执行失败了",
        sql: sqlCode,
        error: String(e),
        timestamp: Date.now(),
      });
      const newItem: QueryHistoryItem = {
        id: Date.now().toString(),
        sql: sqlCode,
        connection_id: selectedConnection?.id || "",
        connection_name: selectedConnection?.name || "",
        timestamp: Math.floor(Date.now() / 1000),
        execution_time_ms: 0,
        success: false,
        error: err.code,
      };
      setHistory((prev) => [newItem, ...prev].slice(0, 100));
    }
  };

  // T-010 + T-034 格式化：后端已升级为词法感知格式化（保留字符串字面量与注释）
  const formatSQL = async () => {
    try {
      const formatted = await invoke<string>("format_sql", { sql: sqlCode });
      setSqlCode(formatted);
    } catch (e: any) {
      msgApi.error(`格式化失败: ${e}`);
    }
  };

  // 复制
  const copy = (text: string) => {
    navigator.clipboard.writeText(text).then(
      () => msgApi.success("已复制"),
      () => msgApi.error("复制失败")
    );
  };

  // 转换为后端 DBConfig（Rust 侧字段为 db_type）
  const toBackendConfig = (c: DBConnection) => ({
    id: c.id,
    name: c.name,
    db_type: c.type,
    host: c.host,
    port: Number(c.port) || 0,
    username: c.username,
    password: c.password,
    database: c.database,
  });

  // 报表工作台要一次拿到所有连接的凭据（跨库 join 的每个源各自定位连接）。
  // 必须 memo：workbench 以 configs 为依赖去拉表清单，每次渲染换引用就会狂刷库。
  const backendConfigs = useMemo(() => connections.map((c) => toBackendConfig(c)), [connections]);
  const aiConfigForReport = useMemo(
    () => ({ base_url: aiConfig.baseUrl, api_key: aiConfig.apiKey, model: aiConfig.model }),
    [aiConfig]
  );

  // 连接配置落盘
  const persistConnections = (list: DBConnection[]) => {
    invoke("save_connections", { connections: list.map(toBackendConfig) }).catch((e) =>
      msgApi.error(`保存连接配置失败: ${e}`)
    );
  };

  // 启动时加载落盘的连接配置
  useEffect(() => {
    invoke<any[]>("load_connections")
      .then((list) =>
        setConnections(
          (list || []).map((c: any) => ({
            id: c.id ?? String(Date.now()),
            name: c.name ?? "",
            type: c.db_type ?? c.type ?? "",
            host: c.host ?? "",
            port: Number(c.port) || 0,
            username: c.username ?? "",
            password: c.password ?? "",
            database: c.database ?? "",
          }))
        )
      )
      .catch(() => {});
  }, []);

  // 加载 AI 配置
  useEffect(() => {
    invoke<any>("get_ai_config")
      .then((config) => {
        if (config && config.base_url) {
          setAiConfig({
            baseUrl: config.base_url,
            apiKey: config.api_key,
            model: config.model || "xop3qwencodernext",
          });
        }
      })
      .catch(() => {});
  }, []);

  // 一次问数用的表目录。列清单读不到的表要丢掉：没有列清单，后端的列校验对那张表形同虚设。
  // 「生成 SQL」按钮与 Agent 对话共用这一份，两边给模型的字段必须一模一样。
  const buildAiCatalog = useCallback(
    async (
      names: string[],
      cfg: BackendConfig
    ): Promise<{ catalog: CatalogTable[]; failed: string[] }> => {
      const built = await Promise.all(
        names.map(async (name) => {
          const schema = tables.find((t) => t.name === name)?.schema || "";
          try {
            const cols = await reportDescribeColumns(cfg, schema, name);
            const entry: CatalogTable = {
              connection_id: cfg.id,
              connection_name: cfg.name,
              database_type: cfg.db_type,
              schema,
              table: name,
              ...catalogColumns(cols),
            };
            return { entry, failed: "" };
          } catch (e: any) {
            return { entry: null, failed: `${name}：${e}` };
          }
        })
      );
      return {
        catalog: built.map((b) => b.entry).filter((t): t is CatalogTable => t !== null),
        failed: built.map((b) => b.failed).filter(Boolean),
      };
    },
    [tables]
  );

  // 多轮改稿：追问（"再按月拆开""把金额换成税前的"）得把上一稿带回去，
  // 否则模型每轮都在从零重写，用户只能自己动手改。上一稿只在同一份表目录下可复用——
  // 换了连接或换了表，旧稿引用的字段可能根本不在本次目录里，带着只会误导。
  const aiPriorRef = useRef<{ key: string; question: string; sql: string } | null>(null);
  const catalogKey = (catalog: CatalogTable[]) =>
    catalog.map((t) => `${t.connection_id}/${t.schema}/${t.table}`).sort().join(",");

  const generateSql = async (question: string, catalog: CatalogTable[], fix?: SqlFix) => {
    const key = catalogKey(catalog);
    const last = aiPriorRef.current;
    // 照错误改时，底稿就是被本机挡下的那一条（连同拒因一起回喂）；
    // 普通生成才走"上一稿"那套：同一句需求再点一次 = 重来一次，不该把上一稿喂回去让它"改"自己
    const prior = fix
      ? { question: fix.question, sql: fix.sql }
      : last && last.key === key && last.question !== question
        ? { question: last.question, sql: last.sql }
        : null;
    const draft = await aiSqlGenerate(
      question,
      catalog,
      aiConfigForReport,
      undefined,
      prior,
      fix?.error ?? null
    );
    // 只成功之后才把这一稿记成新的上一稿：这一轮失败时，旧稿仍是可用的底稿
    aiPriorRef.current = { key, question, sql: draft.sql };
    return {
      draft,
      builtOn: prior && !fix ? prior.question : null,
      // 上一稿还在、却因为范围变了没被用上，得说清楚不是"接着改"
      droppedPrior: !!last && !prior && last.key !== key,
      fixed: !!fix,
    };
  };

  // AI 生成 SQL：自然语言 → 一条过了本机校验的 SQL（后端只看得见我们给的表与列）
  // fix 来自错误卡上那一键：把拒因和被挡下的那条 SQL 一起交回去，改完仍过同一套校验
  const handleAiGenerateSql = async (fix?: SqlFix) => {
    const question = aiNaturalLanguage.trim();
    if (!question) {
      msgApi.warning("请输入自然语言描述");
      return;
    }
    if (!selectedConnection) {
      msgApi.warning("请先连接数据库");
      return;
    }
    if (!aiConfig.baseUrl || !aiConfig.apiKey || !aiConfig.model) {
      msgApi.warning("先配置 AI 服务地址、密钥与模型");
      setShowAiConfigModal(true);
      return;
    }
    // 没显式选表就退回左侧当前选中的那张：至少是用户自己点的表
    const wanted = aiGenTables.length ? aiGenTables : selectedTable ? [selectedTable] : [];
    if (!wanted.length) {
      msgApi.warning("先勾选这次要用的表，模型没有列清单就只能编字段");
      return;
    }
    const cfg = toBackendConfig(selectedConnection);
    setAiLoading(true);
    setAiDraft(null);
    setAiDraftError(null);
    try {
      const { catalog, failed } = await buildAiCatalog(wanted, cfg);
      if (!catalog.length) {
        setAiDraftError({
          title: "列清单没读到，这一稿根本没发给模型",
          detail: `没能读到任何表的列清单：\n${failed.join("\n")}`,
        });
        msgApi.error("生成失败：列清单读不出来");
        return;
      }
      if (failed.length) msgApi.warning(`这些表读不到列清单，本次没带上：${failed.join("；")}`);
      const { draft, builtOn, droppedPrior, fixed } = await generateSql(question, catalog, fix);
      setAiDraft(draft);
      setSqlCode(draft.sql);
      msgApi.success(
        [
          fixed
            ? "照本机拒因在被拒的那条 SQL 上改"
            : builtOn
              ? `在上一稿（${builtOn}）基础上改`
              : droppedPrior
                ? "本次表目录和上一稿不同，这一稿是从零写的"
                : "",
          draft.repairs > 0
            ? `已生成，本机校验打回 ${draft.repairs} 次后通过`
            : "已生成并通过本机校验",
        ]
          .filter(Boolean)
          .join("；")
      );
    } catch (e: any) {
      // 后端把本机校验的拒因原样带回来，别只留一句"失败"。
      // 拒因里点到哪一列、哪张表，只有连同被挡下的那条 SQL 一起回喂才改得动——
      // 模型是单发的，下一轮根本看不见自己刚写的那条。
      const rej = asSqlReject(e);
      const gone = rej.sql?.trim();
      const next: SqlFix | undefined = gone ? { question, error: rej.error, sql: gone } : undefined;
      const hint = next
        ? "\n\n（可以点右上角「让 AI 照这条错误改」：把上面这条错误单和被挡下的那条 SQL 一起交回去）"
        : "";
      setAiDraftError({
        title: "本机拒绝了这一稿",
        detail: `${rej.error}${hint}`,
        fix: next,
      });
      msgApi.error(next ? "没过本机校验，可以照这条错误改" : "生成失败");
    } finally {
      setAiLoading(false);
    }
  };

  /** 拒因点名的表到底躺在哪个连接里。只在"真有不认得的表 + 确实存在别的连接"时才多这一趟；
   *  只认已经读到表清单的连接，问不到就说问不到。SQL 助手的错误卡与 Agent 气泡共用这一份。 */
  const findTablesElsewhere = async (
    names: string[]
  ): Promise<{
    found: { table: string; connection: string }[];
    missing: string[];
    failed: string[];
  } | null> => {
    const others = connections.filter((c) => c.id !== selectedConnection?.id);
    if (!names.length || !others.length) return null;
    const found: { table: string; connection: string }[] = [];
    const failed: string[] = [];
    const hit = new Set(names.map((n) => n.toLowerCase()));
    for (const c of others) {
      let list: TableSummary[] = [];
      try {
        list = await listTables(toBackendConfig(c));
      } catch {
        failed.push(c.name);
        continue;
      }
      for (const t of list) {
        const qualified = t.schema ? `${t.schema}.${t.name}` : t.name;
        for (const n of names) {
          if (!hit.has(n.toLowerCase())) continue;
          if (
            t.name.toLowerCase() === n.toLowerCase() ||
            qualified.toLowerCase() === n.toLowerCase()
          ) {
            hit.delete(n.toLowerCase());
            found.push({ table: n, connection: c.name || c.database || c.id });
          }
        }
      }
    }
    // 一张都没点到、也没哪个连接问失败——那就是"哪儿都没有"，模型编的，改稿那条腿才对症
    if (!found.length && !failed.length) return null;
    const missing = names.filter((n) => !found.some((f) => f.table === n));
    return { found, missing, failed };
  };

  // 错误卡立起来之后，去别的连接问一遍表清单：只在真撞上"表不认得"且确实有别的连接时才多
  // 这一趟，平时不开这个口。清单是异步回来的，用户可能在这段时间里又生成了一稿或关了卡片，
  // 所以要拿序号认回最新那一轮，别让上一轮的结论挂在新卡片上。
  const crossDbSeq = useRef(0);
  useEffect(() => {
    if (!aiDraftError) {
      crossDbSeq.current += 1;
      setCrossDb(null);
      return;
    }
    const names = unknownTablesOfVerdict(aiDraftError.detail);
    if (!names.length) {
      crossDbSeq.current += 1;
      setCrossDb(null);
      return;
    }
    const seq = ++crossDbSeq.current;
    void findTablesElsewhere(names).then((res) => {
      // 清单是异步回来的，这段时间里可能又生成了一稿或关了卡片：只认最新那一轮
      if (crossDbSeq.current === seq) setCrossDb(res);
    });
  }, [aiDraftError, connections, selectedConnection]);

  // 四条解说链路（优化/解释/诊断/结果）只回文字，不动数据：
  // 会改库的那两条另有本机校验与审批门禁。
  const runAiText = async (call: () => Promise<string>) => {
    if (!aiConfig.baseUrl || !aiConfig.apiKey || !aiConfig.model) {
      msgApi.warning("先配置 AI 服务地址、密钥与模型");
      setShowAiConfigModal(true);
      return;
    }
    setAiLoading(true);
    setAiTextError("");
    setAiResult("");
    try {
      setAiResult(await call());
      msgApi.success("AI 已作答");
    } catch (e: any) {
      // 后端把 HTTP 状态码与响应原文一起带回来，别只留一句"服务错误"
      setAiTextError(String(e));
      msgApi.error("AI 没答上来");
    } finally {
      setAiLoading(false);
    }
  };

  // 索引建议得看得懂表：左侧选中了哪张表，就把它的真实字段结构一起给模型
  const schemaForAi = async (): Promise<ColumnInfo[]> => {
    if (!selectedConnection || !selectedTable) return [];
    if (columns.length) return columns;
    try {
      const cols = await invoke<ColumnInfo[]>("get_table_structure", {
        tableName: selectedTable,
        config: toBackendConfig(selectedConnection),
        schema: null,
      });
      setColumns(cols);
      return cols;
    } catch (e: any) {
      msgApi.warning(`读 ${selectedTable} 的字段结构失败：${e}`);
      return [];
    }
  };

  const handleAiOptimizeSql = async () => {
    if (!aiSqlForOptimize.trim()) {
      msgApi.warning("请输入要优化的 SQL");
      return;
    }
    const schema = await schemaForAi();
    if (!schema.length) msgApi.warning("没带表结构，索引建议只能是泛泛而谈");
    await runAiText(() => aiOptimizeSql(aiSqlForOptimize, schema, aiConfigForReport));
  };

  const handleAiExplainSql = async () => {
    if (!aiSqlForExplain.trim()) {
      msgApi.warning("请输入要解释的 SQL");
      return;
    }
    await runAiText(() => aiExplainSql(aiSqlForExplain, aiConfigForReport));
  };

  const handleAiDiagnoseError = async () => {
    if (!aiErrorMessage.trim() || !aiSqlForError.trim()) {
      msgApi.warning("请输入错误信息和 SQL");
      return;
    }
    await runAiText(() => aiDiagnoseError(aiErrorMessage, aiSqlForError, aiConfigForReport));
  };

  const handleAiExplainResults = async () => {
    if (queryResults.length === 0) {
      msgApi.warning("请先执行查询");
      return;
    }
    // T-049：只喂前 100 行样例，不把整张结果表交给模型
    const sample = queryResults.slice(0, 100);
    await runAiText(() => aiExplainResults(sqlCode, sample, aiConfigForReport));
  };

  // 对话这一轮能用到哪些表：和「生成 SQL」页同一条回退规则，两边不能各说各话
  const agentTables = aiGenTables.length ? aiGenTables : selectedTable ? [selectedTable] : [];
  const agentHint = !selectedConnection
    ? "未连接数据库：先在左侧连上，模型只能查已连上的那个库"
    : `已连 ${selectedConnection.name}（${selectedConnection.type}）· 本轮可选表：${
        agentTables.join("、") || "未选"
      }`;

  // Agent 对话的真实实现（T-073）：面板只交过来意图、那句话和最小上下文，连接、表目录、
  // 编辑器当前 SQL 这些只有这里拿得到。两条链路都不碰库：
  // query 出的 SQL 要用户自己点填进编辑器再运行，diagnose 只回文字。
  const agentAsk = async (
    intent: AgentIntent,
    text: string,
    ctx?: AgentAskContext
  ): Promise<AgentResponse> => {
    if (!aiConfig.baseUrl || !aiConfig.apiKey || !aiConfig.model) {
      setShowAiConfigModal(true);
      return {
        success: false,
        content: "",
        error: "先配置 AI 服务地址、密钥与模型（配置窗口已打开）",
      };
    }
    if (intent === "report") {
      // 出图这条链跑在报表工作台那边，结果要由它自己回话：
      // 气泡不能替它宣布成功——挑表、起草、取数每一步都可能被本机挡下
      const outcome = await new Promise<string>((resolve) => {
        let settled = false;
        const fin = (o: string) => {
          if (settled) return;
          settled = true;
          resolve(o);
        };
        // 不换走模式：问数的人要留在对话框里看这一轮的结果，工作台挂在背后跑
        goReport(text, true, fin, false);
        // 三个模型往返加一次取数，慢的时候真能过半分钟；等不到就说等不到
        setTimeout(() => fin("还没跑完"), 40000);
      });
      return {
        success: true,
        content:
          outcome === "已出图"
            ? "已按这句需求出图：跨库挑表 → 起草 → 过本机校验 → 取数渲染。切到顶栏「AI 报表」就是这张板，上面还能点「让 AI 讲清这张图」问口径。"
            : outcome === "出了但有缺口"
              ? "图出来了，但不是所有数据集都取到了数：哪几张集没数、因此少画了哪些组件，看板上都列着，" +
                "那张卡上能直接「再取一次数」重跑整张报表。别把这张板当全量读。"
              : "这次没出图：" + outcome + "。进度、原因和重试入口都在 AI 报表页。",
      };
    }
    if (!selectedConnection) {
      return { success: false, content: "", error: "先在左侧连上数据库：模型只能查已连上的那个库" };
    }
    try {
      if (intent === "diagnose") {
        // 手打的报错按编辑器当前那段诊断；从失败现场点进来的用当时那条语句——
        // 报错之后用户又改了稿子，拿新内容去问旧报错只会问出不相干的答案
        const target = (ctx?.sql ?? sqlCode).trim();
        if (!target) {
          return {
            success: false,
            content: "",
            error: "没有可诊断的 SQL：编辑器当前那段和报错那条都是空的",
          };
        }
        const answer = await aiDiagnoseError(text, target, aiConfigForReport);
        const note =
          ctx?.sql && ctx.sql.trim() !== sqlCode.trim()
            ? "（问的是报错时那条语句，编辑器现在的内容没参与）\n"
            : "";
        return { success: true, content: note + answer };
      }
      if (!agentTables.length) {
        return {
          success: false,
          content: "",
          error: "先勾选这次要用的表（或在左侧选中一张），模型没有列清单就只能编字段",
        };
      }
      const { catalog, failed } = await buildAiCatalog(agentTables, toBackendConfig(selectedConnection));
      if (!catalog.length) {
        return {
          success: false,
          content: "",
          error: `这一稿根本没发给模型：列清单一张都没读到\n${failed.join("\n")}`,
        };
      }
      const { draft, builtOn, droppedPrior, fixed } = await generateSql(text, catalog, ctx?.fix);
      const lines = [
        fixed
          ? "照本机拒因在被挡下的那条 SQL 上改，没从零重写。"
          : builtOn
            ? `在上一稿（当时需求：${builtOn}）基础上改，没从零重写。`
            : droppedPrior
              ? "本次表目录和上一稿不同，这一稿是从零写的。"
              : "",
        `只用 ${catalog.length} 张表的真实字段生成，引用了 ${
          draft.tables.join("、") || "（没引用目录里的表）"
        }，方言 ${draft.dialect}。`,
        draft.repairs > 0 ? `本机校验打回 ${draft.repairs} 次后才通过。` : "一次就过了本机校验。",
      ].filter(Boolean);
      if (failed.length) lines.push(`读不到列清单、这次没带上：${failed.join("；")}`);
      if (draft.warnings.length) lines.push(`提示：${draft.warnings.join("；")}`);
      // 不直接盖编辑器：面板留了「填进编辑器」，什么时候落由用户定
      return { success: true, content: lines.join("\n"), sql: draft.sql };
    } catch (e: any) {
      // 后端把 HTTP 状态码与本机校验的拒因原样带回来，别只留一句"失败"。
      // 被拒时抛的是 SqlReject 对象，直接 String() 会打成 [object Object]。
      const rej = asSqlReject(e);
      // 有被挡下的那条才给改稿单：模型没给出 SQL 时（请求没发出去、回复里没 SQL）
      // 两半缺一半，那一键只会让模型凭空重画
      const gone = rej.sql?.trim();
      // "这张表其实在别的连接里"这一条 SQL 腿改不动：本腿只把当前连接的表交给模型。
      // 认得出真身就把另一条腿的入口给出来，认不出就只留拒因（不编出口）。
      const cross = await findTablesElsewhere(unknownTablesOfVerdict(rej.error));
      // 那句"其实它在别的连接里"由面板按 cross 单独画：错误原文要保持原样，
      // 「诊断这条报错」把整段话喂回模型会连自己加的提示一起诊断
      return {
        success: false,
        content: "",
        error: gone ? `${rej.error}\n被挡下的那条是：\n${gone}` : rej.error,
        fix: gone ? { question: text, error: rej.error, sql: gone } : undefined,
        cross: cross?.found.length
          ? {
              question: text,
              tables: cross.found.map((f) => f.table),
              connections: [...new Set(cross.found.map((f) => f.connection))],
            }
          : undefined,
      };
    }
  };

  // 只登记一次转发函数：agentAsk 每次渲染都是新闭包（要读当前编辑器内容），
  // 直接登记会让 store 跟着每次按键换引用。
  const agentAskRef = useRef(agentAsk);
  agentAskRef.current = agentAsk;
  const askAgent = useCallback(
    (intent: AgentIntent, text: string, ctx?: AgentAskContext) =>
      agentAskRef.current(intent, text, ctx),
    []
  );
  useEffect(() => {
    useAgentStore.getState().setHandler(askAgent);
    return () => useAgentStore.getState().setHandler(null);
  }, [askAgent]);

  /** 把用户送到报表工作台，并把他刚那句需求带过去：
   *  一条 SQL 只能进一个库，跨库要在那边按各库取数、本机内存 join。 */
  /** 把人送到报表工作台（switchMode=false 就只把工作台挂在背后，人留在对话框里），
   *  run=true 时那一轮跑完由 done 回话——气泡不能替链子宣布成功。 */
  const goReport = (
    q?: string,
    run = false,
    done?: (o: ChainOutcome) => void,
    switchMode = true
  ) => {
    setReportSeen(true);
    if (switchMode) setMode("report");
    const body = (q || "").trim();
    if (body) setReportSeed({ q: body, seq: ++reportSeedSeq.current, run, done });
  };

  // 打开 AI 助手：三条 SQL 输入默认用编辑器当前内容，手抄一遍没有意义
  const openAiAssistant = () => {
    const cur = sqlCode.trim();
    if (cur) {
      setAiSqlForOptimize((v) => v || cur);
      setAiSqlForExplain((v) => v || cur);
      setAiSqlForError((v) => v || cur);
    }
    setShowAiResultModal(true);
  };

  // 保存 AI 配置
  const handleSaveAiConfig = async (values: any) => {
    await invoke("save_ai_config", {
      config: {
        base_url: values.baseUrl,
        api_key: values.apiKey,
        model: values.model,
      },
    });
    setAiConfig(values);
    setShowAiConfigModal(false);
    msgApi.success("AI 配置已保存");
  };

  // 复制

  // 连接数据库（真实连接 + 加载表列表）
  const connectDB = async (connection: DBConnection) => {
    const cfg = toBackendConfig(connection);
    try {
      await invoke("test_connection", { config: cfg });
    } catch (e: any) {
      msgApi.error(`连接失败: ${e}`);
      return;
    }
    setSelectedConnection(connection);
    setIsConnected(true);
    setTables([]);
    setSelectedTable(null);
    setColumns([]);
    msgApi.success(`已连接到 ${connection.name}`);

    // Agent 不再存一份"上下文"副本：它那一轮问数直接读这里的 selectedConnection
    // 与勾选表，两份状态迟早会对不上（T-073）。
    try {
      const list = await invoke<TableInfo[]>("get_tables", { config: cfg });
      setTables(list);
    } catch (e: any) {
      msgApi.warning(`获取表列表失败: ${e}`);
    }
  };

  // 刷新表列表
  const refreshTables = async () => {
    if (!selectedConnection) return;
    try {
      const list = await invoke<TableInfo[]>("get_tables", {
        config: toBackendConfig(selectedConnection),
      });
      setTables(list);
      msgApi.success(`已刷新，共 ${list.length} 张表`);
    } catch (e: any) {
      msgApi.error(`刷新失败: ${e}`);
    }
  };

  // 选中表并加载字段结构
  const selectTable = async (tableName: string, schemaName?: string) => {
    setSelectedTable(tableName);
    if (!selectedConnection) return;
    try {
      const cols = await invoke<ColumnInfo[]>("get_table_structure", {
        tableName,
        config: toBackendConfig(selectedConnection),
        schema: schemaName ?? null,
      });
      setColumns(cols);
    } catch (e: any) {
      setColumns([]);
      msgApi.warning(`获取表结构失败: ${e}`);
    }
  };

  // 断开连接
  const disconnectDB = () => {
    setIsConnected(false);
    setSelectedConnection(null);
    setQueryResults([]);
    msgApi.info("已断开连接");
  };

  // 保存连接
  const saveConnection = (values: any) => {
    if (editingConnection) {
      const next = connections.map((c) =>
        c.id === editingConnection.id ? { ...c, ...values } : c
      );
      setConnections(next);
      persistConnections(next);
      msgApi.success("连接已更新");
    } else {
      const newConnection: DBConnection = {
        id: Date.now().toString(),
        ...values,
      };
      const next = [...connections, newConnection];
      setConnections(next);
      persistConnections(next);
      msgApi.success("连接已创建");
    }
    setShowConnectionModal(false);
    setEditingConnection(null);
    form.resetFields();
  };

  // 编辑连接
  const editConnection = (connection: DBConnection) => {
    setEditingConnection(connection);
    form.setFieldsValue(connection);
    setShowConnectionModal(true);
  };

  // 删除连接
  const deleteConnection = (id: string) => {
    Modal.confirm({
      title: "确认删除",
      content: "确定要删除这个连接吗？",
      onOk: () => {
        const next = connections.filter((c) => c.id !== id);
        setConnections(next);
        persistConnections(next);
        msgApi.success("连接已删除");
      },
    });
  };

  // 导出连接
  const handleExport = () => {
    try {
      const data = {
        version: 1,
        exported_at: Date.now() / 1000,
        connections: connections.map((c) => ({
          name: c.name,
          db_type: c.type,
          host: c.host,
          port: c.port,
          username: c.username,
          database: c.database,
        })),
      };
      const json = JSON.stringify(data, null, 2);
      const blob = new Blob([json], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `connections-${new Date().toISOString().slice(0, 10)}.json`;
      a.click();
      URL.revokeObjectURL(url);
      msgApi.success("连接已导出");
    } catch (e: any) {
      msgApi.error("导出失败: " + e);
    }
  };

  // T-037：导入连接配置
  const handleImportFile = async () => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = "application/json,.json";
    input.onchange = async () => {
      const file = input.files?.[0];
      if (!file) return;
      setImportError("");
      setImportPreview([]);
      try {
        const text = await file.text();
        const list = await invoke<ExportedConnection[]>("import_connections", {
          json: text,
          options: { limit: 100, max_bytes: 256 * 1024, check_driver: true },
        });
        setImportPreview(list);
        setShowImportModal(true);
      } catch (e: any) {
        setImportError(String(e));
        setShowImportModal(true);
      }
    };
    input.click();
  };

  const confirmImport = async () => {
    if (importPreview.length === 0) return;
    try {
      const existingNames = new Set(connections.map((c) => c.name));
      const toAdd = importPreview.map((c) => ({
        id: crypto.randomUUID(),
        name: existingNames.has(c.name) ? `${c.name} (导入)` : c.name,
        type: c.db_type as DBConnection["type"],
        host: c.host,
        port: c.port,
        username: c.username,
        password: "",
        database: c.database,
      }));
      const merged = [...connections, ...toAdd];
      await invoke("save_connections", { connections: merged });
      setConnections(merged);
      setShowImportModal(false);
      setImportPreview([]);
      msgApi.success(`已导入 ${toAdd.length} 个连接`);
    } catch (e: any) {
      const err = classifyError(String(e));
      msgApi.error(errorDescription(err));
    }
  };

  // 切换标签页
  const switchTab = (id: string) => {
    const current = tabs.find((t) => t.id === activeTabId);
    if (current) {
      setTabs(tabs.map((t) => (t.id === activeTabId ? { ...t, sql: sqlCode } : t)));
    }
    setActiveTabId(id);
    const target = tabs.find((t) => t.id === id);
    if (target) setSqlCode(target.sql);
  };

  // 新增标签页
  const addTab = () => {
    const newId = `tab-${Date.now()}`;
    const newNum = tabs.length + 1;
    setTabs([...tabs, { id: newId, title: `查询 ${newNum}`, sql: "" }]);
    setActiveTabId(newId);
    setSqlCode("");
  };

  // 关闭标签页
  const closeTab = (id: string) => {
    if (tabs.length === 1) {
      msgApi.warning("至少保留一个标签页");
      return;
    }
    const newTabs = tabs.filter((t) => t.id !== id);
    setTabs(newTabs);
    if (activeTabId === id) {
      setActiveTabId(newTabs[0].id);
      setSqlCode(newTabs[0].sql);
    }
  };

  // 保存常用查询
  const saveAsSavedQuery = (values: any) => {
    const now = Math.floor(Date.now() / 1000);
    if (editingSavedQuery) {
      setSavedQueries(
        savedQueries.map((q) =>
          q.id === editingSavedQuery.id ? { ...q, ...values, updated_at: now } : q
        )
      );
      msgApi.success("已更新");
    } else {
      const newQ: SavedQuery = {
        id: `sq-${Date.now()}`,
        name: values.name,
        sql: values.sql || sqlCode,
        description: values.description || null,
        tags: values.tags || [],
        created_at: now,
        updated_at: now,
      };
      setSavedQueries([...savedQueries, newQ]);
      msgApi.success("已保存到常用查询");
    }
    setShowSaveQueryModal(false);
    setEditingSavedQuery(null);
  };

  // 加载保存的查询
  const loadSavedQuery = (q: SavedQuery) => {
    setSqlCode(q.sql);
    msgApi.success(`已加载: ${q.name}`);
  };

  // T-002：表格列定义使用后端 column_meta，不再用 Object.keys 推导列名
  // 零行结果依旧展示列头（A01 / A02）
  // T-044: 添加单元格编辑支持（双击编辑，change set 跟踪变更）
  const resultColumns =
    queryColumnsMeta.length > 0
      ? queryColumnsMeta.map((meta) => ({
          title: (
            <span>
              {meta.name}
              <Tag style={{ marginLeft: 6 }} color="blue">
                {meta.logical_type}
              </Tag>
            </span>
          ),
          dataIndex: `col_${meta.ordinal}`,
          key: `col_${meta.ordinal}`,
          ellipsis: true,
          onCell: (record: any, rowIndex?: number) => ({
            onDoubleClick: () => {
              if (rowIndex !== undefined && writeAccessEnabled) {
                const cellValue = record[`col_${meta.ordinal}`];
                setEditingCell({ rowIndex, colIndex: meta.ordinal });
                setEditValue(cellValue?.value ?? "");
              }
            },
            style: { cursor: writeAccessEnabled ? "pointer" : "default" },
          }),
          render: (_: any, record: any) => {
            const cellKey = `${record.key}_${meta.ordinal}`;
            const isEditing =
              editingCell?.rowIndex === record.key && editingCell?.colIndex === meta.ordinal;

            if (isEditing) {
              return (
                <Input
                  size="small"
                  value={editValue}
                  onChange={(e) => setEditValue(e.target.value)}
                  onPressEnter={() => {
                    const newCell: TaggedCell = { __kind: "text", value: editValue };
                    const changeKey = cellKey;
                    setChangeSet((prev) => new Map(prev).set(changeKey, newCell));
                    setEditingCell(null);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Escape") setEditingCell(null);
                  }}
                  autoFocus
                  style={{ width: "100%", fontSize: 12 }}
                />
              );
            }

            const isChanged = changeSet.has(cellKey);
            return (
              <span
                style={
                  isChanged
                    ? {
                        backgroundColor: "rgba(82, 196, 26, 0.15)",
                        padding: "1px 4px",
                        borderRadius: 2,
                      }
                    : {}
                }
              >
                {cellDisplay(record[`col_${meta.ordinal}`])}
              </span>
            );
          },
        }))
      : [];

  return (
    <ConfigProvider
      theme={{
        algorithm: darkMode ? theme.darkAlgorithm : theme.defaultAlgorithm,
      }}
    >
      <Layout style={{ height: "100vh" }}>
        <Sider
          width={280}
          theme={darkMode ? "dark" : "light"}
          style={{
            background: cardBgGradient,
            borderRight: `1px solid var(--ant-color-border-secondary)`,
          }}
        >
          <div
            style={{
              padding: "16px",
              borderBottom: `1px solid var(--ant-color-border-secondary)`,
              display: "flex",
              alignItems: "center",
              gap: 12,
            }}
          >
            <DatabaseOutlined
              style={{
                fontSize: "20px",
                background: brandGradient,
                WebkitBackgroundClip: "text",
                WebkitTextFillColor: "transparent",
              }}
            />
            <strong
              style={{
                background: brandGradient,
                WebkitBackgroundClip: "text",
                WebkitTextFillColor: "transparent",
              }}
            >
              数据库连接
            </strong>
          </div>
          <div style={{ padding: "8px" }}>
            <Space orientation="vertical" style={{ width: "100%" }}>
              <Button
                type="primary"
                icon={<PlusOutlined />}
                block
                onClick={() => {
                  setEditingConnection(null);
                  form.resetFields();
                  setShowConnectionModal(true);
                }}
                style={{
                  borderRadius: 8,
                  background: brandGradient,
                  boxShadow: "0 4px 12px rgba(102,126,234,0.3)",
                }}
              >
                新建连接
              </Button>
              <Button
                icon={<DownloadOutlined />}
                block
                onClick={handleExport}
                disabled={connections.length === 0}
                style={{ borderRadius: 8 }}
              >
                导出配置
              </Button>
              <Button
                icon={<UploadOutlined />}
                block
                onClick={() => handleImportFile()}
                style={{ borderRadius: 8 }}
              >
                导入配置
              </Button>
            </Space>
          </div>
          <Menu
            mode="inline"
            theme={darkMode ? "dark" : "light"}
            style={{
              borderRight: 0,
              background: "transparent",
            }}
            items={connections.map((conn) => ({
              key: conn.id,
              icon: <DatabaseOutlined />,
              label: (
                <div
                  style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}
                >
                  <span>{conn.name}</span>
                  <Space size="small">
                    <Tooltip title="编辑">
                      <EditOutlined
                        onClick={(e) => {
                          e.stopPropagation();
                          editConnection(conn);
                        }}
                        style={{ color: "#1890ff" }}
                      />
                    </Tooltip>
                    <Tooltip title="删除">
                      <DeleteOutlined
                        onClick={(e) => {
                          e.stopPropagation();
                          deleteConnection(conn.id);
                        }}
                        style={{ color: "#ff4d4f" }}
                      />
                    </Tooltip>
                  </Space>
                </div>
              ),
              onClick: () => connectDB(conn),
            }))}
          />
        </Sider>
        <Layout>
          <Header
            style={{
              background: darkMode ? "#141414" : "#fff",
              padding: "0 24px",
              borderBottom: `1px solid var(--ant-color-border-secondary)`,
              display: "flex",
              justifyContent: "space-between",
              alignItems: "center",
            }}
          >
            <Space>
              <Segmented
                value={mode}
                onChange={(v) => {
                  const next = v as "sql" | "report";
                  setMode(next);
                  if (next === "report") setReportSeen(true);
                }}
                options={[
                  { label: "SQL 查询", value: "sql", icon: <CodeOutlined /> },
                  { label: "AI 报表", value: "report", icon: <DashboardOutlined /> },
                ]}
              />
              {isConnected && selectedConnection && (
                <>
                  <Tag color="green">已连接</Tag>
                  <span>{selectedConnection.name}</span>
                  <Button size="small" onClick={disconnectDB}>
                    断开连接
                  </Button>
                </>
              )}
            </Space>
            <Space>
              <Text type="secondary">{selectedConnection?.host || "未连接"}</Text>
              <Switch
                checked={darkMode}
                onChange={setDarkMode}
                checkedChildren="🌙"
                unCheckedChildren="☀️"
              />
              <Space>
                <Tooltip title="AI 助手">
                  <Button type="primary" icon={<RobotOutlined />} onClick={openAiAssistant} />
                </Tooltip>
                <Tooltip title="AI 服务设置">
                  <SettingOutlined
                    style={{ fontSize: "16px", cursor: "pointer" }}
                    onClick={() => setShowAiConfigModal(true)}
                  />
                </Tooltip>
              </Space>
            </Space>
          </Header>
          <Content style={{ display: "flex", flexDirection: "column" }}>
            {reportSeen && (
              <div
                style={{
                  flex: 1,
                  minHeight: 0,
                  display: mode === "report" ? "block" : "none",
                }}
              >
                <ReportWorkbench
                  configs={backendConfigs}
                  aiConfig={aiConfigForReport}
                  onOpenAiSettings={() => setShowAiConfigModal(true)}
                  seed={reportSeed}
                />
              </div>
            )}
            {mode === "report" ? null : isConnected ? (
              <>
                {/* 多标签页 */}
                <div
                  style={{
                    padding: "8px 16px 0",
                    borderBottom: `1px solid var(--ant-color-border-secondary)`,
                    background: cardBgGradient,
                    borderRadius: 16,
                    marginBottom: 12,
                  }}
                >
                  <Tabs
                    type="editable-card"
                    activeKey={activeTabId}
                    onChange={switchTab}
                    onEdit={(targetKey, action) => {
                      if (action === "add") addTab();
                      else if (action === "remove") closeTab(String(targetKey));
                    }}
                    items={tabs.map((t) => ({
                      key: t.id,
                      label: t.title,
                      closable: tabs.length > 1,
                    }))}
                  />
                </div>

                {/* 主体三栏布局 */}
                <div style={{ display: "flex", flex: 1, minHeight: 0 }}>
                  {/* 左：表结构 */}
                  <div
                    style={{
                      width: "240px",
                      borderRight: `1px solid var(--ant-color-border-secondary)`,
                      overflow: "auto",
                      padding: 8,
                    }}
                  >
                    <Card
                      size="small"
                      title={
                        <Space>
                          <TableOutlined />
                          表结构{tables.length > 0 && ` (${tables.length})`}
                        </Space>
                      }
                      extra={
                        <Tooltip title="刷新表列表">
                          <Button
                            size="small"
                            type="text"
                            icon={<SyncOutlined />}
                            onClick={refreshTables}
                          />
                        </Tooltip>
                      }
                      style={{
                        borderRadius: 12,
                        background: cardBgGradient,
                      }}
                    >
                      {tables.map((t) => (
                        <div
                          key={t.name}
                          onClick={() => selectTable(t.name)}
                          style={{
                            padding: "6px 8px",
                            cursor: "pointer",
                            background:
                              selectedTable === t.name ? "rgba(102,126,234,0.1)" : undefined,
                            borderRadius: 6,
                            transition: "all 0.2s cubic-bezier(0.4, 0, 0.2, 1)",
                          }}
                          onMouseEnter={(e) => {
                            if (selectedTable !== t.name) {
                              (e.currentTarget as HTMLElement).style.background =
                                "rgba(102,126,234,0.05)";
                            }
                          }}
                          onMouseLeave={(e) => {
                            if (selectedTable !== t.name) {
                              (e.currentTarget as HTMLElement).style.background = "transparent";
                            }
                          }}
                        >
                          <Space orientation="vertical" size={0}>
                            <strong>{t.name}</strong>
                            <Text type="secondary" style={{ fontSize: 11 }}>
                              ~{t.row_estimate} 行 · {((t.size_bytes || 0) / 1024).toFixed(0)} KB
                            </Text>
                          </Space>
                        </div>
                      ))}
                      {selectedTable && (
                        <div
                          style={{
                            marginTop: 12,
                            paddingTop: 12,
                            borderTop: `1px dashed var(--ant-color-border-secondary)`,
                          }}
                        >
                          <Text strong>{selectedTable} 字段：</Text>
                          {columns.map((c) => (
                            <div key={c.name} style={{ marginTop: 6, fontSize: 12 }}>
                              <Space size={4}>
                                {c.is_primary && (
                                  <Tag color="gold" style={{ margin: 0 }}>
                                    PK
                                  </Tag>
                                )}
                                <span style={{ fontWeight: 500 }}>{c.name}</span>
                              </Space>
                              <div
                                style={{ color: "var(--ant-color-text-secondary)", marginLeft: 20 }}
                              >
                                {c.data_type}
                                {!c.nullable && <span style={{ marginLeft: 4 }}>NOT NULL</span>}
                              </div>
                            </div>
                          ))}
                        </div>
                      )}
                    </Card>
                  </div>

                  {/* 中：SQL 编辑器 + 结果 */}
                  <div style={{ flex: 1, display: "flex", flexDirection: "column", padding: 8 }}>
                    {/* SQL 编辑器 */}
                    <Card
                      size="small"
                      style={{
                        marginBottom: 8,
                        flex: "0 0 auto",
                        borderRadius: 12,
                        background: cardBgGradient,
                      }}
                      title={
                        <span
                          style={{
                            fontWeight: 600,
                            background: brandGradient,
                            WebkitBackgroundClip: "text",
                            WebkitTextFillColor: "transparent",
                            backgroundImage: brandGradient,
                          }}
                        >
                          SQL 编辑器
                        </span>
                      }
                      extra={
                        <Space>
                          <Tooltip title="保存为常用查询">
                            <Button
                              size="small"
                              icon={<StarOutlined />}
                              onClick={() => {
                                setEditingSavedQuery(null);
                                setShowSaveQueryModal(true);
                              }}
                              style={{ borderRadius: 6 }}
                            >
                              收藏
                            </Button>
                          </Tooltip>
                          <Button
                            size="small"
                            icon={<FormatPainterOutlined />}
                            onClick={formatSQL}
                            style={{ borderRadius: 6 }}
                          >
                            格式化
                          </Button>
                          <Button
                            size="small"
                            icon={<CopyOutlined />}
                            onClick={() => copy(sqlCode)}
                            style={{ borderRadius: 6 }}
                          >
                            复制
                          </Button>
                          <Tooltip
                            title={
                              writeAccessEnabled ? "可写：可执行 DML/DDL" : "只读：禁止写入，DB-06"
                            }
                          >
                            <Switch
                              size="small"
                              checked={writeAccessEnabled}
                              onChange={setWriteAccessEnabled}
                              checkedChildren="可写"
                              unCheckedChildren="只读"
                              style={{ marginRight: 4 }}
                            />
                          </Tooltip>
                          <Tooltip title="界面行数上限：只把前 N 行送进表格与 CSV（0 = 不限）。后端仍会取完这一次结果，真要少取请在语句里自己收口。">
                            <Space size={2}>
                              <InputNumber
                                size="small"
                                min={0}
                                step={500}
                                value={rowCap}
                                onChange={(v) => setRowCap(Number(v ?? 0))}
                                style={{ width: 86 }}
                              />
                              <Text type="secondary" style={{ fontSize: 12 }}>
                                行
                              </Text>
                            </Space>
                          </Tooltip>
                          <Button
                            type="primary"
                            size="small"
                            icon={<PlayCircleOutlined />}
                            onClick={executeQuery}
                            style={{
                              borderRadius: 6,
                              background: brandGradient,
                              boxShadow: "0 4px 12px rgba(102,126,234,0.3)",
                            }}
                          >
                            执行
                          </Button>
                        </Space>
                      }
                    >
                      {/* T-029: CodeMirror 6 SQL 编辑器 */}
                      <SqlEditor value={sqlCode} onChange={setSqlCode} height="150px" />
                    </Card>

                    {/* 查询结果 */}
                    <Card
                      size="small"
                      style={{
                        flex: 1,
                        overflow: "hidden",
                        borderRadius: 12,
                        background: cardBgGradient,
                      }}
                      title={
                        <Space size={6}>
                          {resultMeta
                            ? `查询结果 (前 ${resultMeta.shown} 行${
                                resultMeta.total ? ` / 共 ${resultMeta.total} 行` : " · 后面还有"
                              })`
                            : `查询结果 (${queryResults.length} 行)`}
                          {resultMeta && (
                            <Tooltip
                              title={`界面上限 ${rowCap} 行：只有前 ${resultMeta.shown} 行进表格与 CSV${
                                resultMeta.total
                                  ? `，这次结果共 ${resultMeta.total} 行`
                                  : "；sqlite 这条是取到上限就停，总行数没数过"
                              }`}
                            >
                              <Tag color="orange">按上限截断</Tag>
                            </Tooltip>
                          )}
                        </Space>
                      }
                      extra={
                        <Space>
                          {changeSet.size > 0 && (
                            <Tag color="green">已修改 {changeSet.size} 项</Tag>
                          )}
                          <Button
                            size="small"
                            icon={<SaveOutlined />}
                            onClick={handleExportResults}
                            style={{ borderRadius: 6 }}
                          >
                            导出
                          </Button>
                          {changeSet.size > 0 && writeAccessEnabled && (
                            <Button
                              size="small"
                              type="primary"
                              onClick={() => {
                                msgApi.info(`已记录 ${changeSet.size} 项变更（待后端提交）`);
                                setChangeSet(new Map());
                              }}
                              style={{ borderRadius: 6, background: brandGradient }}
                            >
                              提交变更
                            </Button>
                          )}
                        </Space>
                      }
                    >
                      {/* T-032: 虚拟滚动结果网格 */}
                      <Table
                        columns={resultColumns}
                        dataSource={queryResults.map((row, index) => {
                          // T-001：按 ordinal 映射到列，便于大结果/同名列不丢数据
                          const obj: Record<string, TaggedCell> & { key: number } = {
                            key: index,
                          } as any;
                          row.forEach((cell, ord) => {
                            obj[`col_${ord}`] = cell;
                          });
                          return obj;
                        })}
                        size="small"
                        virtual
                        scroll={{ x: "max-content", y: 400 }}
                        pagination={{
                          pageSize: 100,
                          showSizeChanger: true,
                          pageSizeOptions: ["50", "100", "200", "500"],
                        }}
                        style={{ borderRadius: 8 }}
                      />
                    </Card>
                  </div>

                  {/* 右：历史 + 收藏 */}
                  <div
                    style={{
                      width: "280px",
                      borderLeft: `1px solid var(--ant-color-border-secondary)`,
                      overflow: "auto",
                    }}
                  >
                    <Tabs
                      size="small"
                      style={{ padding: 8 }}
                      items={[
                        {
                          key: "history",
                          label: (
                            <span>
                              <HistoryOutlined style={{ color: brandGradient }} />
                              历史
                            </span>
                          ),
                          children: (
                            <div style={{ maxHeight: "calc(100vh - 200px)", overflow: "auto" }}>
                              {/* T-035: 历史搜索和过滤 */}
                              <Input
                                placeholder="搜索 SQL 或连接名称..."
                                prefix={<SearchOutlined style={{ color: "#aaa" }} />}
                                size="small"
                                allowClear
                                style={{ marginBottom: 8 }}
                                onChange={(e) => setHistorySearch(e.target.value.toLowerCase())}
                              />
                              {history
                                .filter((h) => {
                                  if (!historySearch) return true;
                                  return (
                                    h.sql.toLowerCase().includes(historySearch) ||
                                    h.connection_name.toLowerCase().includes(historySearch)
                                  );
                                })
                                .map((h) => (
                                  <div
                                    key={h.id}
                                    onClick={() => setSqlCode(h.sql)}
                                    style={{
                                      padding: 8,
                                      cursor: "pointer",
                                      borderBottom: `1px solid var(--ant-color-border-secondary)`,
                                      transition: "all 0.2s cubic-bezier(0.4, 0, 0.2, 1)",
                                    }}
                                    onMouseEnter={(e) => {
                                      (e.currentTarget as HTMLElement).style.background =
                                        "rgba(102,126,234,0.05)";
                                    }}
                                    onMouseLeave={(e) => {
                                      (e.currentTarget as HTMLElement).style.background =
                                        "transparent";
                                    }}
                                  >
                                    <Space
                                      orientation="vertical"
                                      size={2}
                                      style={{ width: "100%" }}
                                    >
                                      <Space
                                        style={{ width: "100%", justifyContent: "space-between" }}
                                      >
                                        <Tag
                                          color={h.success ? "success" : "error"}
                                          style={{ margin: 0 }}
                                        >
                                          {h.execution_time_ms}ms
                                        </Tag>
                                        <Text type="secondary" style={{ fontSize: 11 }}>
                                          {new Date(h.timestamp * 1000).toLocaleTimeString()}
                                        </Text>
                                      </Space>
                                      <Text
                                        code
                                        style={{ fontSize: 11, display: "block" }}
                                        ellipsis
                                      >
                                        {h.sql}
                                      </Text>
                                    </Space>
                                  </div>
                                ))}
                              {history.length === 0 && <Empty description="暂无查询历史" />}
                            </div>
                          ),
                        },
                        {
                          key: "saved",
                          label: (
                            <span>
                              <StarFilled style={{ color: "#faad14" }} />
                              常用
                            </span>
                          ),
                          children: (
                            <div style={{ maxHeight: "calc(100vh - 200px)", overflow: "auto" }}>
                              {savedQueries.map((q) => (
                                <div
                                  key={q.id}
                                  style={{
                                    padding: 8,
                                    cursor: "pointer",
                                    borderBottom: `1px solid var(--ant-color-border-secondary)`,
                                    transition: "all 0.2s cubic-bezier(0.4, 0, 0.2, 1)",
                                  }}
                                  onMouseEnter={(e) => {
                                    (e.currentTarget as HTMLElement).style.background =
                                      "rgba(102,126,234,0.05)";
                                  }}
                                  onMouseLeave={(e) => {
                                    (e.currentTarget as HTMLElement).style.background =
                                      "transparent";
                                  }}
                                >
                                  <Space orientation="vertical" size={2} style={{ width: "100%" }}>
                                    <Space
                                      style={{ width: "100%", justifyContent: "space-between" }}
                                    >
                                      <strong style={{ fontSize: 13 }}>{q.name}</strong>
                                      <Space size={4}>
                                        <EditOutlined
                                          onClick={(e) => {
                                            e.stopPropagation();
                                            setEditingSavedQuery(q);
                                            setShowSaveQueryModal(true);
                                          }}
                                        />
                                        <DeleteOutlined
                                          onClick={(e) => {
                                            e.stopPropagation();
                                            setSavedQueries(
                                              savedQueries.filter((s) => s.id !== q.id)
                                            );
                                            msgApi.success("已删除");
                                          }}
                                        />
                                      </Space>
                                    </Space>
                                    <Text
                                      code
                                      style={{ fontSize: 11 }}
                                      ellipsis
                                      onClick={() => loadSavedQuery(q)}
                                    >
                                      {q.sql}
                                    </Text>
                                    <Space size={4} wrap>
                                      {q.tags.map((t) => (
                                        <Tag
                                          key={t}
                                          color="blue"
                                          style={{ fontSize: 10, margin: 0 }}
                                        >
                                          {t}
                                        </Tag>
                                      ))}
                                    </Space>
                                  </Space>
                                </div>
                              ))}
                              {savedQueries.length === 0 && <Empty description="暂无常用查询" />}
                            </div>
                          ),
                        },
                      ]}
                    />
                  </div>
                </div>
              </>
            ) : (
              <div
                style={{
                  display: "flex",
                  flexDirection: "column",
                  justifyContent: "center",
                  alignItems: "center",
                  height: "100%",
                  color: "var(--ant-color-text-secondary)",
                }}
              >
                <DatabaseOutlined
                  style={{
                    fontSize: "64px",
                    marginBottom: "16px",
                    background: brandGradient,
                    WebkitBackgroundClip: "text",
                    WebkitTextFillColor: "transparent",
                  }}
                />
                <h2
                  style={{
                    background: brandGradient,
                    WebkitBackgroundClip: "text",
                    WebkitTextFillColor: "transparent",
                  }}
                >
                  数据库管理工具
                </h2>
                <p>请从左侧选择一个连接或创建新连接</p>
              </div>
            )}
          </Content>
        </Layout>
      </Layout>

      {msgContext}

      {/* 连接编辑弹窗 */}
      <Modal
        title={editingConnection ? "编辑连接" : "新建连接"}
        open={showConnectionModal}
        onOk={() => form.submit()}
        onCancel={() => {
          setShowConnectionModal(false);
          setEditingConnection(null);
          form.resetFields();
        }}
      >
        <Form form={form} layout="vertical" onFinish={saveConnection}>
          <Form.Item name="name" label="连接名称" rules={[{ required: true }]}>
            <Input placeholder="例如：本地 MySQL" />
          </Form.Item>
          <Form.Item name="type" label="数据库类型" rules={[{ required: true }]}>
            <Select
              placeholder="选择数据库类型"
              options={[
                { value: "mysql", label: "MySQL" },
                { value: "postgresql", label: "PostgreSQL" },
                { value: "sqlite", label: "SQLite" },
                {
                  value: "sqlserver",
                  label: "SQL Server（暂不支持）",
                  disabled: true,
                },
              ]}
            />
          </Form.Item>
          <Form.Item
            noStyle
            shouldUpdate={(prev, cur) =>
              prev?.type !== cur?.type || prev?.database !== cur?.database
            }
          >
            {() => {
              const t = form.getFieldValue("type");
              const isSqlite = t === "sqlite";
              return (
                <>
                  {!isSqlite && (
                    <Form.Item name="host" label="主机地址">
                      <Input placeholder="例如：localhost（SQLite 可留空）" />
                    </Form.Item>
                  )}
                  {!isSqlite && (
                    <Form.Item name="port" label="端口">
                      <Input type="number" placeholder="例如：3306（SQLite 可留空）" />
                    </Form.Item>
                  )}
                  {!isSqlite && (
                    <Form.Item name="username" label="用户名" rules={[{ required: !isSqlite }]}>
                      <Input placeholder="例如：root" />
                    </Form.Item>
                  )}
                  {!isSqlite && (
                    <Form.Item name="password" label="密码">
                      <Input.Password placeholder="输入密码" />
                    </Form.Item>
                  )}
                  <Form.Item
                    name="database"
                    label={isSqlite ? "数据库文件" : "数据库名"}
                    rules={[{ required: true }]}
                  >
                    <Input
                      placeholder={
                        isSqlite
                          ? "选择或填写 .sqlite / .db 文件绝对路径"
                          : "例如：my_database（SQLite 填文件路径）"
                      }
                    />
                  </Form.Item>
                </>
              );
            }}
          </Form.Item>
        </Form>
      </Modal>

      {/* 保存查询弹窗 */}
      <Modal
        title={editingSavedQuery ? "编辑常用查询" : "保存为常用查询"}
        open={showSaveQueryModal}
        onOk={() => {
          const form2 = document.getElementById("save-query-form") as HTMLFormElement;
          form2?.dispatchEvent(new Event("submit", { cancelable: true }));
        }}
        onCancel={() => {
          setShowSaveQueryModal(false);
          setEditingSavedQuery(null);
        }}
      >
        <Form
          id="save-query-form"
          layout="vertical"
          onFinish={saveAsSavedQuery}
          initialValues={editingSavedQuery || { sql: sqlCode, tags: [] }}
        >
          <Form.Item name="name" label="名称" rules={[{ required: true }]}>
            <Input placeholder="例如：查询活跃用户" />
          </Form.Item>
          <Form.Item name="sql" label="SQL">
            <TextArea rows={3} />
          </Form.Item>
          <Form.Item name="description" label="描述">
            <Input placeholder="可选" />
          </Form.Item>
          <Form.Item name="tags" label="标签">
            <Select mode="tags" placeholder="回车添加标签" />
          </Form.Item>
        </Form>
      </Modal>

      {/* AI 配置弹窗 */}
      <Modal
        title="AI 助手配置"
        open={showAiConfigModal}
        onCancel={() => {
          setShowAiConfigModal(false);
          aiForm.resetFields();
        }}
        footer={null}
        width={500}
      >
        <Form
          form={aiForm}
          layout="vertical"
          onFinish={handleSaveAiConfig}
          initialValues={aiConfig}
        >
          <Alert
            title="AI 助手功能"
            description="配置 AI 服务参数后，可使用自然语言生成 SQL、SQL 优化、错误诊断等功能。"
            type="info"
            style={{ marginBottom: 16 }}
          />

          <Form.Item name="baseUrl" label="API 地址" rules={[{ required: true }]}>
            <Input placeholder="例如：https://maas-coding-api.cn-huabei-1.xf-yun.com/v2" />
          </Form.Item>

          <Form.Item name="apiKey" label="API Key" rules={[{ required: true }]}>
            <Input.Password placeholder="格式：APIKey:APISecret" />
          </Form.Item>

          <Form.Item name="model" label="模型" rules={[{ required: true }]}>
            <Input placeholder="例如：xop3qwencodernext" />
          </Form.Item>

          <Form.Item style={{ textAlign: "right", marginBottom: 0 }}>
            <Button type="primary" htmlType="submit">
              保存配置
            </Button>
          </Form.Item>
        </Form>
      </Modal>

      {/* AI 结果弹窗 */}
      <Modal
        title={
          <Space>
            <RobotOutlined />
            AI 助手
          </Space>
        }
        open={showAiResultModal}
        onCancel={() => setShowAiResultModal(false)}
        width={800}
        footer={null}
        styles={{ body: { maxHeight: "70vh", overflow: "auto" } }}
      >
        <Tabs
          activeKey={aiActiveTab}
          onChange={setAiActiveTab}
          items={[
            {
              key: "agent",
              label: (
                <Space>
                  <RobotOutlined />
                  Agent 问数
                </Space>
              ),
              children: (
                <div style={{ height: 480 }}>
                  <AgentPanel
                    hint={agentHint}
                    onClear={() => {
                      // 会话清空了还拿上一轮那稿去"改"，用户看不出串台，只会觉得模型在装
                      aiPriorRef.current = null;
                    }}
                    onUseSql={(sql) => {
                      setSqlCode(sql);
                      msgApi.success("已填进编辑器，运行前自己过一眼");
                    }}
                    onGoReport={(q) => {
                      setShowAiResultModal(false);
                      goReport(q, true);
                    }}
                  />
                </div>
              ),
            },
            {
              key: "generate",
              label: (
                <Space>
                  <SnippetsOutlined />
                  生成 SQL
                </Space>
              ),
              children: (
                <div style={{ marginTop: 16 }}>
                  <Form layout="vertical">
                    <Form.Item
                      label="这次要用的表"
                      help={
                        aiGenTables.length || !selectedTable
                          ? "模型只能在这些表的真实字段里挑，编出来的字段会被本机挡下"
                          : `没选就用左侧当前选中的 ${selectedTable}`
                      }
                    >
                      <Select
                        mode="multiple"
                        style={{ width: "100%" }}
                        value={aiGenTables}
                        onChange={setAiGenTables}
                        maxTagCount="responsive"
                        optionFilterProp="label"
                        placeholder={
                          tables.length
                            ? "选择参与本次查询的表"
                            : "当前连接还没有表清单，先在左侧连上数据库"
                        }
                        options={tables.map((t) => ({
                          value: t.name,
                          label: t.schema ? `${t.schema}.${t.name}` : t.name,
                        }))}
                      />
                    </Form.Item>
                    <Form.Item label="自然语言描述">
                      <TextArea
                        value={aiNaturalLanguage}
                        onChange={(e) => setAiNaturalLanguage(e.target.value)}
                        placeholder="用自然语言描述你的需求，例如：查询所有活跃用户，按创建时间排序"
                        rows={4}
                      />
                    </Form.Item>
                    <Button
                      type="primary"
                      icon={<BulbOutlined />}
                      onClick={() => handleAiGenerateSql()}
                      loading={aiLoading}
                      disabled={!selectedConnection}
                      block
                    >
                      {aiLoading ? "生成中（每次尝试最长 60 秒）" : "生成 SQL"}
                    </Button>
                  </Form>
                  {/* 与其让用户撞了墙再找出口，不如在撞之前就说清这一腿的边界在哪 */}
                  {connections.length > 1 && (
                    <div style={{ marginTop: 10 }}>
                      <Text type="secondary" style={{ fontSize: 12, display: "block", marginBottom: 4 }}>
                        {`这一腿只看得到当前连接（${
                          selectedConnection?.name || "还没连上"
                        }）的表；本机还有 ${
                          connections.length - 1
                        } 条连接的表要一起用，得走报表那条腿：各库分别取数，在本机内存里 join。`}
                      </Text>
                      <Button
                        size="small"
                        icon={<DashboardOutlined />}
                        disabled={!aiNaturalLanguage.trim()}
                        onClick={() => {
                          setShowAiResultModal(false);
                          goReport(aiNaturalLanguage, true);
                        }}
                      >
                        把这句需求拿去跨库出图
                      </Button>
                    </div>
                  )}
                  {aiDraftError && (
                    <Alert
                      type="error"
                      showIcon
                      closable
                      onClose={() => setAiDraftError(null)}
                      style={{ marginTop: 12 }}
                      title={aiDraftError.title}
                      description={
                        <>
                          <div
                            style={{ whiteSpace: "pre-wrap", fontFamily: "monospace", fontSize: 12 }}
                          >
                            {aiDraftError.detail}
                          </div>
                          {/* 点名的表在别的连接里时，"照着错误再改一轮"这条腿是走不通的：
                              本腿的目录只装得下当前连接的表。这时必须给出另一条腿的入口。 */}
                          {crossDb && (
                            <div
                              className="ai-cross-db"
                              style={{
                                marginTop: 8,
                                padding: "8px 10px",
                                borderRadius: 6,
                                borderLeft: "3px solid #1677ff",
                                background: "rgba(22,119,255,0.08)",
                                fontSize: 12,
                              }}
                            >
                              {crossDb.found.length > 0 && (
                                <div>
                                  {/* 中文句子中间换行会被 JSX 折成一个真空格，所以长句按段拼 */}
                                  {crossDb.found
                                    .map((f) => `${f.table} 在连接「${f.connection}」里`)
                                    .join("；") +
                                    "。这一腿只把当前连接的表交给模型，而一条 SQL 也只能进一个库——" +
                                    "这张表进不了本次目录，改多少轮都会以同样的理由被挡下。"}
                                </div>
                              )}
                              {crossDb.missing.length > 0 && (
                                <div style={{ marginTop: crossDb.found.length ? 4 : 0 }}>
                                  {(crossDb.found.length ? "另外点名的 " : "点名的 ") +
                                    crossDb.missing.join("、") +
                                    (crossDb.failed.length
                                      ? ` 在已问到的连接里没有，而 ${crossDb.failed.join(
                                          "、"
                                        )} 的表清单没读到——问不到不等于不存在。`
                                      : " 在所有连接里都没有，那是模型编出来的字段名。")}
                                </div>
                              )}
                              {crossDb.found.length > 0 && (
                                <>
                                  <Text type="secondary" style={{ display: "block", marginTop: 4 }}>
                                    {"不同库的表要进同一张结果，走 AI 报表：每个库各自取数，" +
                                      "在本机内存里 join。下面那一键会带着这句话过去把整条链跑完。"}
                                  </Text>
                                  <Button
                                    size="small"
                                    type="primary"
                                    icon={<DashboardOutlined />}
                                    style={{ marginTop: 6 }}
                                    onClick={() => {
                                      setShowAiResultModal(false);
                                      goReport(aiNaturalLanguage, true);
                                    }}
                                  >
                                    拿去 AI 报表，直接跨库出图
                                  </Button>
                                </>
                              )}
                            </div>
                          )}
                        </>
                      }
                      action={
                        aiDraftError.fix ? (
                          <Tooltip title="把上面的拒因和被挡下的那条 SQL 一起回喂给模型，让它改完再过一遍本机校验">
                            <Button
                              size="small"
                              danger
                              icon={<ThunderboltOutlined />}
                              loading={aiLoading}
                              onClick={() => handleAiGenerateSql(aiDraftError.fix)}
                            >
                              让 AI 照这条错误改
                            </Button>
                          </Tooltip>
                        ) : null
                      }
                    />
                  )}
                  {aiDraft && (
                    <Card
                      size="small"
                      style={{ marginTop: 12 }}
                      title={
                        <Space size={4}>
                          <CheckCircleOutlined style={{ color: "#52c41a" }} />
                          已填入编辑器
                        </Space>
                      }
                      extra={
                        <Space size={4}>
                          <Tag color="blue">{aiDraft.dialect}</Tag>
                          <Tooltip title="模型被本机校验打回并自我修正的次数">
                            <Badge
                              count={aiDraft.repairs}
                              showZero
                              color={aiDraft.repairs > 0 ? "#faad14" : "#52c41a"}
                            />
                          </Tooltip>
                        </Space>
                      }
                    >
                      <pre
                        style={{
                          margin: 0,
                          whiteSpace: "pre-wrap",
                          wordBreak: "break-all",
                          fontSize: 12,
                          fontFamily: "monospace",
                        }}
                      >
                        {aiDraft.sql}
                      </pre>
                      <div style={{ marginTop: 8 }}>
                        <Text type="secondary" style={{ fontSize: 12 }}>
                          依据：
                        </Text>
                        {aiDraft.tables.length ? (
                          aiDraft.tables.map((t) => (
                            <Tag key={t} style={{ marginBottom: 4 }}>
                              {t}
                            </Tag>
                          ))
                        ) : (
                          <Text type="secondary" style={{ fontSize: 12 }}>
                            没有引用目录里的表
                          </Text>
                        )}
                      </div>
                      {aiDraft.warnings.map((w) => (
                        <Alert
                          key={w}
                          type="warning"
                          showIcon
                          title={w}
                          style={{ padding: 4, marginTop: 4 }}
                        />
                      ))}
                      <Space style={{ marginTop: 8 }}>
                        <Button
                          size="small"
                          icon={<CopyOutlined />}
                          onClick={() => copy(aiDraft.sql)}
                        >
                          复制
                        </Button>
                      </Space>
                    </Card>
                  )}
                </div>
              ),
            },
            {
              key: "optimize",
              label: (
                <Space>
                  <CodeOutlined />
                  优化 SQL
                </Space>
              ),
              children: (
                <div style={{ marginTop: 16 }}>
                  <Form layout="vertical">
                    <Form.Item
                      label="要优化的 SQL"
                      help={
                        selectedTable
                          ? `会带上左侧选中表 ${selectedTable} 的 ${columns.length} 个真实字段，索引建议才落到实处`
                          : "先在左侧选中一张表，否则模型看不到表结构，索引建议只能泛泛而谈"
                      }
                    >
                      <TextArea
                        value={aiSqlForOptimize}
                        onChange={(e) => setAiSqlForOptimize(e.target.value)}
                        placeholder="输入要优化的 SQL 语句"
                        rows={6}
                      />
                    </Form.Item>
                    <Button
                      type="primary"
                      icon={<BulbOutlined />}
                      onClick={handleAiOptimizeSql}
                      disabled={aiLoading}
                      block
                    >
                      {aiLoading ? <Spin size="small" /> : "分析并优化"}
                    </Button>
                  </Form>
                </div>
              ),
            },
            {
              key: "explain",
              label: (
                <Space>
                  <ApiOutlined />
                  解释 SQL
                </Space>
              ),
              children: (
                <div style={{ marginTop: 16 }}>
                  <Form layout="vertical">
                    <Form.Item label="要解释的 SQL">
                      <TextArea
                        value={aiSqlForExplain}
                        onChange={(e) => setAiSqlForExplain(e.target.value)}
                        placeholder="输入要解释的 SQL 语句"
                        rows={6}
                      />
                    </Form.Item>
                    <Button
                      type="primary"
                      icon={<BulbOutlined />}
                      onClick={handleAiExplainSql}
                      disabled={aiLoading}
                      block
                    >
                      {aiLoading ? <Spin size="small" /> : "解释执行逻辑"}
                    </Button>
                  </Form>
                </div>
              ),
            },
            {
              key: "diagnose",
              label: (
                <Space>
                  <BugOutlined />
                  错误诊断
                </Space>
              ),
              children: (
                <div style={{ marginTop: 16 }}>
                  <Form layout="vertical">
                    <Form.Item label="错误信息">
                      <Input
                        value={aiErrorMessage}
                        onChange={(e) => setAiErrorMessage(e.target.value)}
                        placeholder="复制的错误信息"
                      />
                    </Form.Item>
                    <Form.Item label="执行的 SQL">
                      <TextArea
                        value={aiSqlForError}
                        onChange={(e) => setAiSqlForError(e.target.value)}
                        placeholder="执行失败的 SQL 语句"
                        rows={4}
                      />
                    </Form.Item>
                    <Button
                      type="primary"
                      icon={<BugOutlined />}
                      onClick={handleAiDiagnoseError}
                      disabled={aiLoading}
                      block
                    >
                      {aiLoading ? <Spin size="small" /> : "诊断问题"}
                    </Button>
                  </Form>
                </div>
              ),
            },
            {
              key: "explainResults",
              label: (
                <Space>
                  <DatabaseOutlined />
                  解释结果
                </Space>
              ),
              children: (
                <div style={{ marginTop: 16 }}>
                  <Alert
                    title="提示"
                    description="执行查询后，可使用此功能分析查询结果"
                    type="info"
                    style={{ marginBottom: 16 }}
                  />
                  <Button
                    type="primary"
                    icon={<BulbOutlined />}
                    onClick={handleAiExplainResults}
                    disabled={aiLoading || queryResults.length === 0}
                    block
                  >
                    {aiLoading ? <Spin size="small" /> : "分析查询结果"}
                  </Button>
                </div>
              ),
            },
          ]}
        />

        {aiTextError && (
          <Alert
            type="error"
            showIcon
            closable
            onClose={() => setAiTextError("")}
            style={{ marginTop: 16 }}
            title="这一条没答上来"
            description={
              <div style={{ whiteSpace: "pre-wrap", fontFamily: "monospace", fontSize: 12 }}>
                {aiTextError}
              </div>
            }
          />
        )}

        {aiResult && (
          <Card
            title="AI 回答"
            style={{ marginTop: 16 }}
            extra={
              <Button size="small" onClick={() => copy(aiResult)}>
                复制
              </Button>
            }
          >
            <Typography style={{ whiteSpace: "pre-wrap", fontFamily: "monospace" }}>
              {aiResult}
            </Typography>
          </Card>
        )}
      </Modal>

      {/* T-045 写入审批 Modal */}
      <Modal
        open={!!pendingApproval}
        title={
          <Space>
            <Tag color="orange">写入审批</Tag>
            <span>确认高风险 SQL</span>
          </Space>
        }
        onCancel={() => {
          approvalPendingRef.current?.(null);
          setPendingApproval(null);
        }}
        okButtonProps={{
          danger: true,
          disabled: !pendingApproval || pendingGrant !== null,
        }}
        okText="授权执行"
        cancelText="取消"
        onOk={() => {
          if (!pendingApproval) return;
          const g = buildGrant(pendingApproval);
          setPendingGrant(g);
          approvalPendingRef.current?.(g);
          setPendingApproval(null);
        }}
        width={640}
      >
        {pendingApproval && (
          <Space orientation="vertical" style={{ width: "100%" }} size="middle">
            <Alert
              type="warning"
              showIcon
              title="此 SQL 包含写入/危险子句，需要显式审批"
              description={
                <span>
                  环境：
                  <Tag color="blue">{pendingApproval.environment}</Tag>
                  TTL：{pendingApproval.ttlSec}s ｜ 仅本次有效 ｜ 摘要绑定 SQL 全文
                </span>
              }
            />
            <Card size="small" title="待执行 SQL">
              <pre style={{ whiteSpace: "pre-wrap", margin: 0, fontSize: 12 }}>
                {pendingApproval.sql}
              </pre>
            </Card>
            <Card size="small" title="校验信息">
              <ul style={{ paddingLeft: 18, margin: 0 }}>
                <li>审批 ID 将自动生成（UUID v4）</li>
                <li>SQL 摘要由前 64 字符 + 长度组成</li>
                <li>超过 TTL 或摘要变化将导致后端拒绝执行</li>
                <li>单次使用，已使用后无法重放</li>
              </ul>
            </Card>
          </Space>
        )}
      </Modal>

      {/* T-037 导入连接 Modal */}
      <Modal
        open={showImportModal}
        title="导入连接配置"
        onCancel={() => setShowImportModal(false)}
        onOk={confirmImport}
        okText="导入所选"
        cancelText="取消"
        okButtonProps={{ disabled: importPreview.length === 0 || !!importError }}
        width={720}
      >
        {importError ? (
          <Alert type="error" showIcon title="导入校验失败" description={importError} />
        ) : (
          <Space orientation="vertical" style={{ width: "100%" }} size="middle">
            <Alert
              type="info"
              showIcon
              title={`将导入 ${importPreview.length} 条连接`}
              description="导入的连接不携带密码；如有重名会自动追加 (导入) 后缀。"
            />
            <Table
              size="small"
              dataSource={importPreview}
              rowKey={(r) => `${r.db_type}-${r.name}-${r.host}`}
              pagination={false}
              columns={[
                { title: "名称", dataIndex: "name", key: "name" },
                { title: "驱动", dataIndex: "db_type", key: "db_type" },
                { title: "主机", dataIndex: "host", key: "host" },
                { title: "端口", dataIndex: "port", key: "port" },
                { title: "用户名", dataIndex: "username", key: "username" },
                { title: "数据库", dataIndex: "database", key: "database" },
              ]}
            />
          </Space>
        )}
      </Modal>
    </ConfigProvider>
  );
}

export default App;
