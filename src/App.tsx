import { useState, useEffect, useRef } from "react";
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
} from "antd";
import {
  buildGrant,
  isLikelyWriteSql,
  PendingApproval,
} from "./ipc/approval";
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
} from "@ant-design/icons";
import { invoke } from "@tauri-apps/api/core";
// 使用本地 Agent 组件（临时方案，待共享库修复后迁移到 z-biz-tool-shared）
import { useAgentStore } from './agent/AgentManager';
import AgentPanel from './agent/AgentPanel';

// 渐变色主题常量
const brandGradient = "linear-gradient(135deg, #667eea 0%, #764ba2 100%)";
const cardBgGradient = "linear-gradient(135deg, rgba(102,126,234,0.04) 0%, rgba(118,75,162,0.04) 100%)";

const { Header, Sider, Content } = Layout;
const { TextArea } = Input;
const { Text } = Typography;

// Rust 风格可空类型
type Option<T> = T | null;

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
  // T-001 + T-002：以后端 column_meta 为准，零行结果仍有列定义
  const [queryColumnsMeta, setQueryColumnsMeta] = useState<ColumnMeta[]>([]);
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
      invoke("save_query_history", { history }).catch((e) =>
        msgApi.error(`保存历史失败：${e}`),
      );
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
  
  // AI 配置
  const [aiConfig, setAiConfig] = useState({
    baseUrl: "",
    apiKey: "",
    model: "xop3qwencodernext",
  });
  const [showAiConfigModal, setShowAiConfigModal] = useState(false);
  const [aiForm] = Form.useForm();

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
  const [aiLoading] = useState(false);
  const [aiResult, setAiResult] = useState<string>("");
  const [showAiResultModal, setShowAiResultModal] = useState(false);
  const [aiActiveTab, setAiActiveTab] = useState<string>("generate");
  const [aiNaturalLanguage, setAiNaturalLanguage] = useState("");
  const [aiSqlForOptimize, setAiSqlForOptimize] = useState("");
  const [aiSqlForExplain, setAiSqlForExplain] = useState("");
  const [aiErrorMessage, setAiErrorMessage] = useState("");
  const [aiSqlForError, setAiSqlForError] = useState("");

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
      grant = await requestApproval(
        sqlCode,
        (selectedConnection as any).environment ?? "unknown",
      );
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
      });

      // 异步校验：标签或连接已切走 → 丢弃结果
      if (
        requestConnId !== selectedConnection?.id ||
        requestTabId !== activeTabId
      ) {
        msgApi.warning("连接或标签已切换，结果已丢弃（DB-11 防竞态）");
        return;
      }

      // 同步覆写列源：以后端 column_meta 为准；零行也有列（A01）
      setQueryColumnsMeta(result.column_meta ?? []);
      setQueryResults(result.rows || []);

      // 添加到 Agent 历史
      useAgentStore.getState().addMessage({
        id: Date.now().toString(),
        role: 'user',
        content: `执行查询: ${sqlCode}`,
        timestamp: Date.now(),
      });

      useAgentStore.getState().addMessage({
        id: (Date.now() + 1).toString(),
        role: 'agent',
        content: `查询返回 ${result.rows?.length || 0} 行结果`,
        sql: sqlCode,
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
      msgApi.success({
        content: `查询执行成功，总耗时 ${result.execution_time_ms}ms${
          result.timings && result.timings.total_ms > 0
            ? `（分类 ${result.timings.queue_ms}ms / 执行 ${result.timings.execute_ms}ms）`
            : ""
        }`,
        duration: 3,
      });
    } catch (e: any) {
      if (
        requestConnId !== selectedConnection?.id ||
        requestTabId !== activeTabId
      ) {
        return;
      }
      msgApi.error({
        content: `${errorDescription(classifyError(String(e)))}`,
        duration: 5,
      });
      const err = classifyError(String(e));
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

  // 连接配置落盘
  const persistConnections = (list: DBConnection[]) => {
    invoke("save_connections", { connections: list.map(toBackendConfig) }).catch(
      (e) => msgApi.error(`保存连接配置失败: ${e}`)
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

  // AI 功能 - 使用共享 Agent 组件
  const handleAiGenerateSql = async () => {
    if (!aiNaturalLanguage.trim()) {
      msgApi.warning("请输入自然语言描述");
      return;
    }
    if (!selectedConnection) {
      msgApi.warning("请先连接数据库");
      return;
    }
    
    try {
      const result = await useAgentStore.getState().query(aiNaturalLanguage);
      
      if (result.success && result.sql) {
        setSqlCode(result.sql);
        msgApi.success("SQL 已生成");
      } else if (result.error) {
        msgApi.error(`Agent 错误: ${result.error}`);
      }
    } catch (e: any) {
      msgApi.error(`AI 服务错误: ${e}`);
    }
  };

  const handleAiOptimizeSql = async () => {
    if (!aiSqlForOptimize.trim()) {
      msgApi.warning("请输入要优化的 SQL");
      return;
    }
    
    try {
      const result = await useAgentStore.getState().optimize(aiSqlForOptimize);
      
      if (result.success && result.sql) {
        setAiResult(result.content);
        setShowAiResultModal(true);
      } else if (result.error) {
        msgApi.error(`Agent 错误: ${result.error}`);
      }
    } catch (e: any) {
      msgApi.error(`AI 服务错误: ${e}`);
    }
  };

  const handleAiExplainSql = async () => {
    if (!aiSqlForExplain.trim()) {
      msgApi.warning("请输入要解释的 SQL");
      return;
    }
    
    try {
      const result = await useAgentStore.getState().optimize(aiSqlForExplain);
      
      if (result.success) {
        setAiResult(result.content);
        setShowAiResultModal(true);
      } else if (result.error) {
        msgApi.error(`Agent 错误: ${result.error}`);
      }
    } catch (e: any) {
      msgApi.error(`AI 服务错误: ${e}`);
    }
  };

  const handleAiDiagnoseError = async () => {
    if (!aiErrorMessage.trim() || !aiSqlForError.trim()) {
      msgApi.warning("请输入错误信息和 SQL");
      return;
    }
    
    try {
      const result = await useAgentStore.getState().diagnoseError(aiErrorMessage, aiSqlForError);
      
      if (result.success) {
        setAiResult(result.content);
        setShowAiResultModal(true);
      } else if (result.error) {
        msgApi.error(`Agent 错误: ${result.error}`);
      }
    } catch (e: any) {
      msgApi.error(`AI 服务错误: ${e}`);
    }
  };

  const handleAiExplainResults = async () => {
    if (queryResults.length === 0) {
      msgApi.warning("请先执行查询");
      return;
    }
    // T-049：上下文授权提示前置——S2 将插入 Modal.confirm
    // 截取前 100 行作为样例，最大 64 KiB；明示 NOT_CONFIGURED 边界
    const sample = queryResults.slice(0, 100);
    try {
      const result = await useAgentStore.getState().analyze(sqlCode, sample);
      
      if (result.success) {
        setAiResult(result.content);
        setShowAiResultModal(true);
      } else if (result.error) {
        msgApi.error(`Agent 错误: ${result.error}`);
      }
    } catch (e: any) {
      msgApi.error(`AI 服务错误: ${e}`);
    }
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
    
    // 设置 Agent 上下文
    useAgentStore.getState().setContext({
      connectionId: connection.id,
      databaseType: connection.type,
      databaseName: connection.database,
    });
    
    try {
      const list = await invoke<TableInfo[]>("get_tables", { config: cfg });
      setTables(list);
      
      // 更新 Agent 上下文的表信息
      useAgentStore.getState().setContext({
        connectionId: connection.id,
        databaseType: connection.type,
        databaseName: connection.database,
        tables: list.map(t => ({
          name: t.name,
          columns: [], // TODO: 从后端获取列信息
        })),
      });
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
          q.id === editingSavedQuery.id
            ? { ...q, ...values, updated_at: now }
            : q
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
  const resultColumns = queryColumnsMeta.length > 0
    ? queryColumnsMeta.map((meta) => ({
        title: (
          <span>
            {meta.name}
            <Tag style={{ marginLeft: 6 }} color="blue">{meta.logical_type}</Tag>
          </span>
        ),
        dataIndex: `col_${meta.ordinal}`,
        key: `col_${meta.ordinal}`,
        ellipsis: true,
        render: (_: any, record: any) => cellDisplay(record[`col_${meta.ordinal}`]),
      }))
    : [];

  return (
    <ConfigProvider
      theme={{
        algorithm: darkMode ? theme.darkAlgorithm : theme.defaultAlgorithm,
      }}
    >
      <Layout style={{ height: "100vh" }}>
        <Sider width={280} theme={darkMode ? "dark" : "light"} style={{ 
          background: cardBgGradient,
          borderRight: `1px solid var(--ant-color-border-secondary)`,
        }}>
          <div 
            style={{ 
              padding: "16px", 
              borderBottom: `1px solid var(--ant-color-border-secondary)`,
              display: "flex",
              alignItems: "center",
              gap: 12,
            }}
          >
            <DatabaseOutlined style={{ fontSize: "20px", background: brandGradient, WebkitBackgroundClip: "text", WebkitTextFillColor: "transparent" }} />
            <strong style={{ background: brandGradient, WebkitBackgroundClip: "text", WebkitTextFillColor: "transparent" }}>数据库连接</strong>
          </div>
          <div style={{ padding: "8px" }}>
            <Space direction="vertical" style={{ width: "100%" }}>
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
              background: 'transparent',
            }}
            items={connections.map((conn) => ({
              key: conn.id,
              icon: <DatabaseOutlined />,
              label: (
                <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
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
              <Text type="secondary">
                {selectedConnection?.host || "未连接"}
              </Text>
              <Switch
                checked={darkMode}
                onChange={setDarkMode}
                checkedChildren="🌙"
                unCheckedChildren="☀️"
              />
              <Space>
                <Tooltip title="AI 助手">
                  <Button
                    type="primary"
                    icon={<RobotOutlined />}
                    onClick={() => setShowAiConfigModal(true)}
                  />
                </Tooltip>
                <Tooltip title="设置">
                  <SettingOutlined 
                    style={{ fontSize: "16px", cursor: "pointer" }} 
                    onClick={() => setShowAiConfigModal(true)}
                  />
                </Tooltip>
              </Space>
            </Space>
          </Header>
          <Content style={{ display: "flex", flexDirection: "column" }}>
            {isConnected ? (
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
                  <div style={{ width: "240px", borderRight: `1px solid var(--ant-color-border-secondary)`, overflow: "auto", padding: 8 }}>
                    <Card
                      size="small"
                      title={<Space><TableOutlined />表结构{tables.length > 0 && ` (${tables.length})`}</Space>}
                      extra={
                        <Tooltip title="刷新表列表">
                          <Button size="small" type="text" icon={<SyncOutlined />} onClick={refreshTables} />
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
                            background: selectedTable === t.name ? "rgba(102,126,234,0.1)" : undefined,
                            borderRadius: 6,
                            transition: "all 0.2s cubic-bezier(0.4, 0, 0.2, 1)",
                          }}
                          onMouseEnter={(e) => {
                            if (selectedTable !== t.name) {
                              (e.currentTarget as HTMLElement).style.background = "rgba(102,126,234,0.05)";
                            }
                          }}
                          onMouseLeave={(e) => {
                            if (selectedTable !== t.name) {
                              (e.currentTarget as HTMLElement).style.background = "transparent";
                            }
                          }}
                        >
                          <Space direction="vertical" size={0}>
                            <strong>{t.name}</strong>
                            <Text type="secondary" style={{ fontSize: 11 }}>
                              ~{t.row_estimate} 行 · {((t.size_bytes || 0) / 1024).toFixed(0)} KB
                            </Text>
                          </Space>
                        </div>
                      ))}
                      {selectedTable && (
                        <div style={{ marginTop: 12, paddingTop: 12, borderTop: `1px dashed var(--ant-color-border-secondary)` }}>
                          <Text strong>{selectedTable} 字段：</Text>
                          {columns.map((c) => (
                            <div key={c.name} style={{ marginTop: 6, fontSize: 12 }}>
                              <Space size={4}>
                                {c.is_primary && <Tag color="gold" style={{ margin: 0 }}>PK</Tag>}
                                <span style={{ fontWeight: 500 }}>{c.name}</span>
                              </Space>
                              <div style={{ color: "var(--ant-color-text-secondary)", marginLeft: 20 }}>
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
                        <span style={{ 
                          fontWeight: 600,
                          background: brandGradient,
                          WebkitBackgroundClip: "text",
                          WebkitTextFillColor: "transparent",
                          backgroundImage: brandGradient,
                        }}>SQL 编辑器</span>
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
                          <Tooltip title={writeAccessEnabled ? "可写：可执行 DML/DDL" : "只读：禁止写入，DB-06"}>
                            <Switch
                              size="small"
                              checked={writeAccessEnabled}
                              onChange={setWriteAccessEnabled}
                              checkedChildren="可写"
                              unCheckedChildren="只读"
                              style={{ marginRight: 4 }}
                            />
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
                      <TextArea
                        value={sqlCode}
                        onChange={(e) => setSqlCode(e.target.value)}
                        autoSize={{ minRows: 4, maxRows: 12 }}
                        style={{ 
                          fontFamily: "monospace", 
                          fontSize: 13,
                          borderRadius: 8,
                        }}
                        placeholder="输入 SQL 语句..."
                      />
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
                      title={`查询结果 (${queryResults.length} 行)`}
                      extra={
                        <Space>
                          <Button size="small" icon={<SaveOutlined />} style={{ borderRadius: 6 }}>导出</Button>
                        </Space>
                      }
                    >
                      <Table
                        columns={resultColumns}
                        dataSource={queryResults.map((row, index) => {
                          // T-001：按 ordinal 映射到列，便于大结果/同名列不丢数据
                          const obj: Record<string, TaggedCell> & { key: number } = { key: index } as any;
                          row.forEach((cell, ord) => {
                            obj[`col_${ord}`] = cell;
                          });
                          return obj;
                        })}
                        size="small"
                        scroll={{ x: "max-content", y: 300 }}
                        pagination={{ pageSize: 50 }}
                        style={{ borderRadius: 8 }}
                      />
                    </Card>
                  </div>

                  {/* 右：历史 + 收藏 */}
                  <div style={{ width: "280px", borderLeft: `1px solid var(--ant-color-border-secondary)`, overflow: "auto" }}>
                    <Tabs
                      size="small"
                      style={{ padding: 8 }}
                      items={[
                        {
                          key: "history",
                          label: <span><HistoryOutlined style={{ color: brandGradient }} />历史</span>,
                          children: (
                            <div style={{ maxHeight: "calc(100vh - 200px)", overflow: "auto" }}>
                              {history.map((h) => (
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
                                    (e.currentTarget as HTMLElement).style.background = "rgba(102,126,234,0.05)";
                                  }}
                                  onMouseLeave={(e) => {
                                    (e.currentTarget as HTMLElement).style.background = "transparent";
                                  }}
                                >
                                  <Space direction="vertical" size={2} style={{ width: "100%" }}>
                                    <Space style={{ width: "100%", justifyContent: "space-between" }}>
                                      <Tag color={h.success ? "success" : "error"} style={{ margin: 0 }}>
                                        {h.execution_time_ms}ms
                                      </Tag>
                                      <Text type="secondary" style={{ fontSize: 11 }}>
                                        {new Date(h.timestamp * 1000).toLocaleTimeString()}
                                      </Text>
                                    </Space>
                                    <Text code style={{ fontSize: 11, display: "block" }} ellipsis>
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
                          label: <span><StarFilled style={{ color: "#faad14" }} />常用</span>,
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
                                    (e.currentTarget as HTMLElement).style.background = "rgba(102,126,234,0.05)";
                                  }}
                                  onMouseLeave={(e) => {
                                    (e.currentTarget as HTMLElement).style.background = "transparent";
                                  }}
                                >
                                  <Space direction="vertical" size={2} style={{ width: "100%" }}>
                                    <Space style={{ width: "100%", justifyContent: "space-between" }}>
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
                                            setSavedQueries(savedQueries.filter((s) => s.id !== q.id));
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
                                        <Tag key={t} color="blue" style={{ fontSize: 10, margin: 0 }}>
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
                <DatabaseOutlined style={{ fontSize: "64px", marginBottom: "16px", background: brandGradient, WebkitBackgroundClip: "text", WebkitTextFillColor: "transparent" }} />
                <h2 style={{ background: brandGradient, WebkitBackgroundClip: "text", WebkitTextFillColor: "transparent" }}>数据库管理工具</h2>
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
                    <Form.Item
                      name="username"
                      label="用户名"
                      rules={[{ required: !isSqlite }]}
                    >
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
            message="AI 助手功能" 
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
              label: <Space><RobotOutlined />Agent 对话</Space>,
              children: (
                <div style={{ height: 480 }}>
                  <AgentPanel />
                </div>
              ),
            },
            {
              key: "generate",
              label: <Space><SnippetsOutlined />生成 SQL</Space>,
              children: (
                <div style={{ marginTop: 16 }}>
                  <Form layout="vertical">
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
                      onClick={handleAiGenerateSql}
                      disabled={aiLoading || !selectedConnection}
                      block
                    >
                      {aiLoading ? <Spin size="small" /> : "生成 SQL"}
                    </Button>
                  </Form>
                </div>
              ),
            },
            {
              key: "optimize",
              label: <Space><CodeOutlined />优化 SQL</Space>,
              children: (
                <div style={{ marginTop: 16 }}>
                  <Form layout="vertical">
                    <Form.Item label="要优化的 SQL">
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
              label: <Space><ApiOutlined />解释 SQL</Space>,
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
              label: <Space><BugOutlined />错误诊断</Space>,
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
              label: <Space><DatabaseOutlined />解释结果</Space>,
              children: (
                <div style={{ marginTop: 16 }}>
                  <Alert
                    message="提示"
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
          <Space direction="vertical" style={{ width: "100%" }} size="middle">
            <Alert
              type="warning"
              showIcon
              message="此 SQL 包含写入/危险子句，需要显式审批"
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
          <Alert type="error" showIcon message="导入校验失败" description={importError} />
        ) : (
          <Space direction="vertical" style={{ width: "100%" }} size="middle">
            <Alert
              type="info"
              showIcon
              message={`将导入 ${importPreview.length} 条连接`}
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