// AI 报表工作台。
//
// 一条链路：选表 → 组目录 → ai_report_draft（模型只写语义规格，本机校验挡住幻觉）
// → report_view_render（跨库取数 + 内存算子链）→ 看板。
// 用户全程不写 SQL，但每一步都能看到下了哪些 SQL、走了哪些算子。

import { useEffect, useMemo, useRef, useState } from "react";
import {
  Alert,
  Badge,
  Button,
  Card,
  Collapse,
  Empty,
  Input,
  Select,
  Space,
  Spin,
  Tabs,
  Tag,
  Tooltip,
  Typography,
  message,
} from "antd";
import {
  BulbOutlined,
  CheckCircleOutlined,
  ReloadOutlined,
  RobotOutlined,
  ThunderboltOutlined,
} from "@ant-design/icons";
import {
  aiReportDraft,
  listTables,
  reportDescribeColumns,
  reportViewRender,
  reportViewValidate,
  type AIConfig,
  type BackendConfig,
  type TableSummary,
} from "./api";
import type { CatalogTable, DraftResult, ViewPayload, ViewSpec, DatasetSpec } from "./types";
import { ReportBoard, SqlList } from "./ReportBoard";

const { Text, Title } = Typography;
const { TextArea } = Input;

/** 目录项的稳定键：连接 + schema + 表 */
const keyOf = (t: { connection_id: string; schema?: string; table: string }) =>
  `${t.connection_id}\u0000${t.schema || ""}\u0000${t.table}`;

interface ConnState {
  loading: boolean;
  tables: TableSummary[];
  error?: string;
}

export function ReportWorkbench({
  configs,
  aiConfig,
  onOpenAiSettings,
}: {
  configs: BackendConfig[];
  aiConfig: AIConfig;
  onOpenAiSettings: () => void;
}) {
  const [msgApi, msgHolder] = message.useMessage();
  const [connState, setConnState] = useState<Record<string, ConnState>>({});
  const [columns, setColumns] = useState<Record<string, string[]>>({});
  const [picked, setPicked] = useState<string[]>([]);
  const [question, setQuestion] = useState("");
  const [draft, setDraft] = useState<DraftResult | null>(null);
  const [specText, setSpecText] = useState("");
  const [payload, setPayload] = useState<ViewPayload | null>(null);
  const [drafting, setDrafting] = useState(false);
  const [rendering, setRendering] = useState(false);
  // 只校验用自己的 loading：它不取数、比渲染便宜，共用一个开关会让
  // "取数并渲染"在检查期间转圈并吞掉点击，看起来像卡住。
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState<string>("");

  // 与查询链路同一套防竞态：晚到的响应不能盖掉新结果
  const runId = useRef(0);
  // 已发出/已拿到的列清单请求。只看 columns 会漏：同一次 onChange 里
  // 逐个 ensure 时 state 还没落地，同一张表会被问两次（真库就是两次探列）。
  const colInFlight = useRef<Set<string>>(new Set());

  const aiReady = Boolean(aiConfig.base_url && aiConfig.api_key && aiConfig.model);

  useEffect(() => {
    let cancelled = false;
    setConnState({});
    (async () => {
      for (const cfg of configs) {
        setConnState((s) => ({ ...s, [cfg.id]: { loading: true, tables: [] } }));
        try {
          const list = await listTables(cfg);
          if (!cancelled) {
            setConnState((s) => ({ ...s, [cfg.id]: { loading: false, tables: list || [] } }));
          }
        } catch (e) {
          if (!cancelled) {
            setConnState((s) => ({
              ...s,
              [cfg.id]: { loading: false, tables: [], error: String(e) },
            }));
          }
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [configs]);

  const catalog: CatalogTable[] = useMemo(() => {
    const byId = new Map(configs.map((c) => [c.id, c]));
    return picked
      .map((k) => {
        const [connId, schema, table] = k.split("\u0000");
        const cfg = byId.get(connId);
        if (!cfg) return null;
        return {
          connection_id: cfg.id,
          connection_name: cfg.name,
          database_type: cfg.db_type,
          schema: schema || "",
          table,
          columns: columns[k] || [],
        } as CatalogTable;
      })
      .filter((t): t is CatalogTable => t !== null);
  }, [picked, configs, columns]);

  const ensureColumns = async (key: string) => {
    if (columns[key] || colInFlight.current.has(key)) return;
    colInFlight.current.add(key);
    const [connId, schema, table] = key.split("\u0000");
    const cfg = configs.find((c) => c.id === connId);
    if (!cfg) {
      colInFlight.current.delete(key);
      return;
    }
    try {
      const cols = await reportDescribeColumns(cfg, schema || "", table);
      setColumns((s) => ({ ...s, [key]: cols }));
    } catch (e) {
      // 失败了要允许重选这张表再问一次，否则它永远没有列清单
      colInFlight.current.delete(key);
      msgApi.error(`${table} 列清单读取失败：${e}`);
    }
  };

  const tableOptions = configs.map((c) => ({
    label: c.name || c.id,
    options: (connState[c.id]?.tables || []).map((t) => ({
      value: keyOf({ connection_id: c.id, schema: t.schema || "", table: t.name }),
      label: `${t.schema ? `${t.schema}.` : ""}${t.name}`,
    })),
  }));

  const applyDraft = (d: DraftResult) => {
    setDraft(d);
    setPayload(null);
    setSpecText(JSON.stringify({ datasets: d.datasets, view: d.view }, null, 2));
  };

  const onDraft = async () => {
    if (!aiReady) {
      onOpenAiSettings();
      return;
    }
    if (catalog.length === 0) {
      msgApi.warning("先选至少一张表，模型没有目录就只能编字段");
      return;
    }
    const id = ++runId.current;
    setDrafting(true);
    setError("");
    try {
      const d = await aiReportDraft(question, catalog, aiConfig);
      if (id !== runId.current) return;
      applyDraft(d);
      msgApi.success(
        d.repairs > 0 ? `已生成，本机校验打回 ${d.repairs} 次后通过` : "已生成并通过本机校验"
      );
    } catch (e) {
      if (id !== runId.current) return;
      setError(String(e));
      msgApi.error("生成失败");
    } finally {
      if (id === runId.current) setDrafting(false);
    }
  };

  const spec = useMemo<{ view?: ViewSpec; datasets?: DatasetSpec[]; parseError?: string }>(() => {
    if (!specText.trim()) return {};
    try {
      const parsed = JSON.parse(specText);
      return { view: parsed.view, datasets: parsed.datasets };
    } catch (e) {
      return { parseError: `JSON 不合法：${e}` };
    }
  }, [specText]);

  const onRender = async () => {
    if (!spec.view || !spec.datasets) {
      msgApi.warning(spec.parseError || "先有一份合法的草稿 JSON");
      return;
    }
    const id = ++runId.current;
    setRendering(true);
    setError("");
    try {
      const p = await reportViewRender(spec.view, spec.datasets, configs);
      if (id !== runId.current) return;
      setPayload(p);
      msgApi.success(`${p.charts.length} 个组件 · ${p.elapsed_ms} ms`);
    } catch (e) {
      if (id !== runId.current) return;
      setError(String(e));
      msgApi.error("渲染失败");
    } finally {
      if (id === runId.current) setRendering(false);
    }
  };

  /** 只跑计划不取数：改完 JSON 先自检一次，比直接渲染便宜得多 */
  const onCheck = async () => {
    if (!spec.view || !spec.datasets) {
      msgApi.warning(spec.parseError || "先有一份合法的草稿 JSON");
      return;
    }
    const id = ++runId.current;
    setChecking(true);
    setError("");
    try {
      const r = await reportViewValidate(spec.view, spec.datasets, configs);
      if (id !== runId.current) return;
      msgApi.success(`校验通过：${r.steps.length} 步算子链，${r.sqls.length} 条下推 SQL`);
    } catch (e) {
      if (id !== runId.current) return;
      setError(String(e));
      msgApi.error("校验未通过");
    } finally {
      if (id === runId.current) setChecking(false);
    }
  };

  const datasetNameOf = (widgetId: string) => {
    const w =
      draft?.view.widgets.find((x) => x.id === widgetId) ||
      spec.view?.widgets.find((x) => x.id === widgetId);
    const dsId = w?.dataset;
    return (draft?.datasets || spec.datasets || []).find((d) => d.id === dsId)?.name;
  };

  return (
    <div style={{ display: "flex", height: "100%", minHeight: 0 }}>
      {msgHolder}
      {/* 左：数据源目录 + 问题 */}
      <div
        style={{
          width: 320,
          padding: 12,
          overflow: "auto",
          borderRight: "1px solid var(--ant-color-border-secondary)",
        }}
      >
        <Title level={5} style={{ marginTop: 0 }}>
          <BulbOutlined /> 报表目录
        </Title>
        <Text type="secondary" style={{ fontSize: 12 }}>
          勾选的表会连列清单一起交给模型；模型只能在这些字段里挑，编出来的字段会被本机挡下。
        </Text>
        <Select
          mode="multiple"
          allowClear
          style={{ width: "100%", margin: "8px 0" }}
          placeholder="选择参与报表的表（可跨连接）"
          value={picked}
          onChange={(v: string[]) => {
            setPicked(v);
            v.forEach(ensureColumns);
          }}
          options={tableOptions}
          optionFilterProp="label"
          maxTagCount="responsive"
        />
        {configs.map((c) => {
          const st = connState[c.id];
          if (!st || st.loading) return null;
          if (st.error) {
            return (
              <Tooltip key={c.id} title={st.error}>
                <Tag color="red" style={{ marginBottom: 4 }}>
                  {c.name || c.id} 读表失败
                </Tag>
              </Tooltip>
            );
          }
          return null;
        })}
        <TextArea
          rows={4}
          value={question}
          onChange={(e) => setQuestion(e.target.value)}
          placeholder="例：按城市统计 2026 年已支付订单的 GMV，并列出金额最高的 5 笔订单"
          style={{ marginBottom: 8 }}
        />
        <Space orientation="vertical" style={{ width: "100%" }}>
          <Button
            type="primary"
            block
            icon={<RobotOutlined />}
            loading={drafting}
            disabled={!aiReady && !question.trim()}
            onClick={onDraft}
          >
            {drafting ? "生成中（每次尝试最长 60 秒）" : "AI 生成报表草稿"}
          </Button>
          {!aiReady && (
            <Button size="small" block onClick={onOpenAiSettings}>
              先配置 AI 服务地址与密钥
            </Button>
          )}
        </Space>
        {draft && (
          <Card
            size="small"
            style={{ marginTop: 12 }}
            title={
              <Space size={4}>
                <CheckCircleOutlined style={{ color: "#52c41a" }} />
                草稿已过本机校验
              </Space>
            }
            extra={
              <Tooltip title="模型被本机校验打回并自我修正的次数">
                <Badge
                  count={draft.repairs}
                  showZero
                  color={draft.repairs > 0 ? "#faad14" : "#52c41a"}
                />
              </Tooltip>
            }
          >
            <Space orientation="vertical" size={4} style={{ width: "100%" }}>
              {draft.datasets.map((d) => (
                <div key={d.id}>
                  <Text strong style={{ fontSize: 12 }}>
                    {d.name}
                  </Text>
                  <div>
                    {d.sources.map((s) => (
                      <Tag key={`${s.alias}`} color={s.connection_id}>
                        {s.alias} · {s.table}
                      </Tag>
                    ))}
                  </div>
                  <Text type="secondary" style={{ fontSize: 11 }}>
                    {(draft.columns[d.id] || []).join(", ") || "（无输出列）"}
                  </Text>
                </div>
              ))}
              {draft.warnings.map((w) => (
                <Alert key={w} type="warning" showIcon title={w} style={{ padding: 4 }} />
              ))}
            </Space>
          </Card>
        )}
      </div>

      {/* 右：看板 / 计划 / SQL / 规格 */}
      <div style={{ flex: 1, minWidth: 0, padding: 12, overflow: "auto" }}>
        {error && (
          <Alert
            type="error"
            showIcon
            closable
            onClose={() => setError("")}
            style={{ marginBottom: 12 }}
            title="本机拒绝了这一稿"
            description={
              <div style={{ whiteSpace: "pre-wrap", fontFamily: "monospace", fontSize: 12 }}>
                {error}
              </div>
            }
          />
        )}
        <Space style={{ marginBottom: 12 }} wrap>
          <Button
            type="primary"
            icon={<ThunderboltOutlined />}
            loading={rendering}
            disabled={!draft && !spec.view}
            onClick={onRender}
          >
            取数并渲染
          </Button>
          <Button
            icon={<CheckCircleOutlined />}
            loading={checking}
            disabled={!spec.view}
            onClick={onCheck}
          >
            只校验
          </Button>
          {payload && (
            <Button
              icon={<ReloadOutlined />}
              onClick={() => {
                setPayload(null);
                setDraft(null);
                setSpecText("");
                setQuestion("");
                runId.current += 1;
              }}
            >
              清空
            </Button>
          )}
          {payload && <Tag color="blue">{payload.elapsed_ms} ms</Tag>}
        </Space>
        {/* Tabs 始终在：空态文案让用户"切到规格 JSON 手搓"，就不能先把手搓那条路藏起来 */}
        <Tabs
          items={[
            {
              key: "board",
              label: "看板",
              children:
                rendering && !payload ? (
                  <Spin style={{ display: "block", margin: "80px auto" }} />
                ) : payload ? (
                  <ReportBoard payload={payload} datasetNameOf={datasetNameOf} />
                ) : draft ? (
                  <Empty description="还没有取数，点「取数并渲染」" style={{ marginTop: 60 }} />
                ) : (
                  <Empty
                    description={
                      <Space orientation="vertical">
                        <Text>还没有草稿。左侧选表并描述需求，或者直接切到「规格 JSON」手搓。</Text>
                        <Text type="secondary" style={{ fontSize: 12 }}>
                          跨库报表在本机内存里 join：不同连接的表可以进同一张图，SQL 只按单表下推。
                        </Text>
                      </Space>
                    }
                    style={{ marginTop: 60 }}
                  />
                ),
            },
            {
              key: "plan",
              label: "执行计划",
              children: (
                <Collapse
                  defaultActiveKey={["plan"]}
                  items={[
                    {
                      key: "plan",
                      label: `算子链 ${(draft?.steps.length || 0) + (payload?.steps.length || 0)} 步`,
                      children: (
                        <pre style={{ margin: 0, fontSize: 12, whiteSpace: "pre-wrap" }}>
                          {(draft?.steps || []).join("\n")}
                          {draft && payload ? "\n" : ""}
                          {(payload?.steps || []).join("\n") || "（渲染后才有视图算子链）"}
                        </pre>
                      ),
                    },
                  ]}
                />
              ),
            },
            {
              key: "sql",
              label: `生成的 SQL ${payload?.generated_sql.length ?? 0}`,
              children: <SqlList sqls={payload?.generated_sql || []} />,
            },
            {
              key: "spec",
              label: "规格 JSON",
              children: (
                <Space orientation="vertical" style={{ width: "100%" }}>
                  <Text type="secondary" style={{ fontSize: 12 }}>
                    改完直接点「只校验」或「取数并渲染」。方言与列清单由本机补齐，写了也会被覆盖。
                  </Text>
                  {spec.parseError && <Alert type="error" showIcon title={spec.parseError} />}
                  <TextArea
                    value={specText}
                    onChange={(e) => setSpecText(e.target.value)}
                    rows={20}
                    style={{ fontFamily: "monospace", fontSize: 12 }}
                    placeholder="草稿会出现在这里，可手工编辑"
                  />
                </Space>
              ),
            },
          ]}
        />
      </div>
    </div>
  );
}

export default ReportWorkbench;
