// AI 报表工作台。
//
// 一条链路：选表 → 组目录 → ai_report_draft（模型只写语义规格，本机校验挡住幻觉）
// → report_view_render（跨库取数 + 内存算子链）→ 看板。
// 用户全程不写 SQL，但每一步都能看到下了哪些 SQL、走了哪些算子。
// 跑通的一稿可以「存入报表簿」（save_report）：存规格而不是结果，
// 下次点开即重新取数，看到的永远是库里当下的数字。

import { useEffect, useMemo, useRef, useState } from "react";
import {
  Alert,
  Badge,
  Button,
  Card,
  Collapse,
  Empty,
  Input,
  List,
  Modal,
  Popconfirm,
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
  DeleteOutlined,
  FolderOpenOutlined,
  ReloadOutlined,
  RobotOutlined,
  SaveOutlined,
  ThunderboltOutlined,
} from "@ant-design/icons";
import {
  aiReportDraft,
  deleteReport,
  listTables,
  loadReports,
  reportDescribeColumns,
  reportViewRender,
  reportViewValidate,
  saveReport,
  type AIConfig,
  type BackendConfig,
  type TableSummary,
} from "./api";
import type {
  CatalogTable,
  DatasetSpec,
  DraftResult,
  SavedReport,
  ViewPayload,
  ViewSpec,
} from "./types";
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
  // 正在读列清单的表：面板要把它和"读失败"分开显示，
  // 否则每勾一张表都会先闪一条红色"列清单没读到"。
  const [colBusy, setColBusy] = useState<string[]>([]);
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
  // 失败分两段：标题说"哪一步拒的"，详情留后端原文。只给一句大字符串的话，
  // 缺连接和幻觉字段会顶同一个标题，用户分不出该去补表还是该改问题。
  const [error, setError] = useState<{ title: string; detail: string } | null>(null);
  const [tab, setTab] = useState("board");

  // 报表簿
  const [reports, setReports] = useState<SavedReport[]>([]);
  // 读失败与"读到了但一条没有"必须分开：把损坏的 reports.json 显示成
  // "报表簿还是空的"，用户会以为存过的东西消失了。
  const [reportsError, setReportsError] = useState("");
  const [reportsLoading, setReportsLoading] = useState(false);
  const [saveOpen, setSaveOpen] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveName, setSaveName] = useState("");
  const [saveDesc, setSaveDesc] = useState("");
  // 当前工作台里这份稿子对应报表簿里的哪一条（空 = 还没存过）
  const [openId, setOpenId] = useState("");
  const [openName, setOpenName] = useState("");
  // 打开/保存那一刻的 spec 原文，用来判断"改过没有"，决定按钮该写更新还是已存
  const [savedSpecText, setSavedSpecText] = useState("");

  // 与查询链路同一套防竞态：晚到的响应不能盖掉新结果
  const runId = useRef(0);
  // 列清单请求按 key 存 promise。用 Set 只够去重，不够等：起草那一刻
  // 在飞的请求还没落地，catalog 里那张表就是 columns: []。
  const colInFlight = useRef<Map<string, Promise<void>>>(new Map());
  // state 是渲染快照，await 之后读它会拿到请求落地前的旧值，
  // 所以列清单同时镜像到 ref，起草时用 ref 现读。
  const columnsRef = useRef<Record<string, string[]>>({});

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

  const refreshReports = async () => {
    setReportsLoading(true);
    try {
      setReports((await loadReports()) || []);
      setReportsError("");
    } catch (e) {
      // 读不出来说明落盘文件坏了，得让用户看见，而不是给一个空列表以为没存过
      setReportsError(String(e));
      msgApi.error(`报表簿读取失败：${e}`);
    } finally {
      setReportsLoading(false);
    }
  };

  useEffect(() => {
    refreshReports();
  }, []);

  /** 目录按"能不能拿去起草"分成两堆：连接已删、列清单为空的表都不能进目录。
   *  空列清单比缺连接更阴：后端 normalize_sources 会把它原样写进 schema 缓存，
   *  之后每次引用该表都报"未声明列清单"，三轮自我修正全烧在这上面。 */
  const splitPicked = (colMap: Record<string, string[]>) => {
    const byId = new Map(configs.map((c) => [c.id, c]));
    const ready: CatalogTable[] = [];
    const unusable: { key: string; table: string; conn: string; why: string }[] = [];
    for (const k of picked) {
      const [connId, schema, table] = k.split("\u0000");
      const cfg = byId.get(connId);
      if (!cfg) {
        // 连接被删过就只剩这个 id 了，截一段出来至少能对上当初配的是哪个
        unusable.push({
          key: k,
          table,
          conn: `${connId.slice(0, 8)}…`,
          why: "所在连接已不在本机",
        });
        continue;
      }
      const cols = colMap[k] || [];
      if (!cols.length) {
        unusable.push({ key: k, table, conn: cfg.name || cfg.id, why: "列清单没读到" });
        continue;
      }
      ready.push({
        connection_id: cfg.id,
        connection_name: cfg.name,
        database_type: cfg.db_type,
        schema: schema || "",
        table,
        columns: cols,
      });
    }
    return { ready, unusable };
  };

  const { ready: catalog, unusable } = useMemo(() => splitPicked(columns), [
    picked,
    configs,
    columns,
  ]);

  const ensureColumns = (key: string): Promise<void> => {
    const running = colInFlight.current.get(key);
    if (running) return running;
    if ((columnsRef.current[key] || []).length) return Promise.resolve();
    const [connId, schema, table] = key.split("\u0000");
    const cfg = configs.find((c) => c.id === connId);
    if (!cfg) return Promise.resolve();
    setColBusy((s) => [...s, key]);
    const p = (async () => {
      try {
        const cols = await reportDescribeColumns(cfg, schema || "", table);
        columnsRef.current = { ...columnsRef.current, [key]: cols };
        setColumns((s) => ({ ...s, [key]: cols }));
      } catch (e) {
        // 失败了要允许重选这张表再问一次，否则它永远没有列清单
        msgApi.error(`${table} 列清单读取失败：${e}`);
      } finally {
        colInFlight.current.delete(key);
        setColBusy((s) => s.filter((k) => k !== key));
      }
    })();
    colInFlight.current.set(key, p);
    return p;
  };

  /** 起草前的前置闸：把在飞的列清单等完，缺的补读一次，然后重新分组 */
  const readyCatalogForDraft = async () => {
    const keys = picked.filter((k) => !(columnsRef.current[k] || []).length);
    if (keys.length) await Promise.allSettled(keys.map(ensureColumns));
    return splitPicked(columnsRef.current);
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
    // 新草稿与之前打开的那条没关系了，保存必须落到新条目上
    setOpenId("");
    setOpenName("");
    setSavedSpecText("");
  };

  const onDraft = async () => {
    if (!aiReady) {
      onOpenAiSettings();
      return;
    }
    if (picked.length === 0) {
      msgApi.warning("先选至少一张表，模型没有目录就只能编字段");
      return;
    }
    const id = ++runId.current;
    setDrafting(true);
    setError(null);
    try {
      const { ready, unusable } = await readyCatalogForDraft();
      if (id !== runId.current) return;
      // 读不到列清单的表要点名丢掉：没有列清单，后端的列校验对那张表形同虚设，
      // 留着只会让模型自由发挥、三轮修复全烧在"未声明列清单"上。
      if (!ready.length) {
        setError({
          title: "列清单没读到，这一稿根本没发给模型",
          detail: unusable.map((u) => `· ${u.conn}/${u.table}：${u.why}`).join("\n"),
        });
        msgApi.error("没有一张表能进目录，先补上连接或重选表");
        return;
      }
      if (unusable.length) {
        msgApi.warning(
          `已丢掉进不了目录的 ${unusable.length} 张表：${unusable
            .map((u) => `${u.table}（${u.why}）`)
            .join("、")}`
        );
      }
      const d = await aiReportDraft(question, ready, aiConfig);
      if (id !== runId.current) return;
      applyDraft(d);
      msgApi.success(
        d.repairs > 0 ? `已生成，本机校验打回 ${d.repairs} 次后通过` : "已生成并通过本机校验"
      );
    } catch (e) {
      if (id !== runId.current) return;
      setError({ title: "本机拒绝了这一稿", detail: String(e) });
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

  /** 抽出来是为了"打开报表即取数"：那时 specText 的 state 还没落地，不能走 onRender */
  const runRender = async (view: ViewSpec, datasets: DatasetSpec[]) => {
    const id = ++runId.current;
    setRendering(true);
    setError(null);
    try {
      const p = await reportViewRender(view, datasets, configs);
      if (id !== runId.current) return;
      setPayload(p);
      msgApi.success(`${p.charts.length} 个组件 · ${p.elapsed_ms} ms`);
    } catch (e) {
      if (id !== runId.current) return;
      // 这一段是真的下了 SQL 到库上，拒因多半来自数据库而不是语义层，标题别说反
      setError({ title: "取数失败", detail: String(e) });
      msgApi.error("渲染失败");
    } finally {
      if (id === runId.current) setRendering(false);
    }
  };

  const onRender = async () => {
    if (!spec.view || !spec.datasets) {
      msgApi.warning(spec.parseError || "先有一份合法的草稿 JSON");
      return;
    }
    await runRender(spec.view, spec.datasets);
  };

  /** 只跑计划不取数：改完 JSON 先自检一次，比直接渲染便宜得多 */
  const onCheck = async () => {
    if (!spec.view || !spec.datasets) {
      msgApi.warning(spec.parseError || "先有一份合法的草稿 JSON");
      return;
    }
    const id = ++runId.current;
    setChecking(true);
    setError(null);
    try {
      const r = await reportViewValidate(spec.view, spec.datasets, configs);
      if (id !== runId.current) return;
      msgApi.success(`校验通过：${r.steps.length} 步算子链，${r.sqls.length} 条下推 SQL`);
    } catch (e) {
      if (id !== runId.current) return;
      setError({ title: "本机拒绝了这一稿", detail: String(e) });
      msgApi.error("校验未通过");
    } finally {
      if (id === runId.current) setChecking(false);
    }
  };

  // ================== 报表簿 ==================

  const specReady = Boolean(spec.view && spec.datasets) && !spec.parseError;
  const dirty = Boolean(openId) && specText !== savedSpecText;

  const openSaveDialog = () => {
    if (!specReady) {
      msgApi.warning(spec.parseError || "先有一份合法的草稿 JSON");
      return;
    }
    setSaveName(openName || spec.view?.name || question.trim().slice(0, 30) || "未命名报表");
    setSaveDesc(reports.find((r) => r.id === openId)?.description || "");
    setSaveOpen(true);
  };

  /** asNew = 另存一份；否则有 openId 就更新那一条 */
  const onSave = async (asNew: boolean) => {
    if (!spec.view || !spec.datasets) return;
    const name = saveName.trim();
    if (!name) {
      msgApi.warning("给报表起个名字，不然报表簿里认不出它");
      return;
    }
    const now = Math.floor(Date.now() / 1000);
    const id = asNew || !openId ? crypto.randomUUID() : openId;
    const item: SavedReport = {
      id,
      name,
      description: saveDesc.trim() || null,
      question,
      datasets: spec.datasets,
      // 视图名跟着报表名走，否则看板标题还是模型起的那句
      view: { ...spec.view, name },
      created_at: now,
      updated_at: now,
    };
    setSaving(true);
    try {
      await saveReport(item);
      setOpenId(id);
      setOpenName(name);
      setSavedSpecText(specText);
      setSaveOpen(false);
      await refreshReports();
      msgApi.success(asNew || !openId ? "已另存为新报表" : "报表已更新");
    } catch (e) {
      // 后端存前会做一次结构体检（布局越栏、组件挂空数据集），拒因要看得见
      msgApi.error(`保存失败：${e}`);
    } finally {
      setSaving(false);
    }
  };

  const onOpenReport = async (r: SavedReport) => {
    const text = JSON.stringify({ datasets: r.datasets, view: r.view }, null, 2);
    const keys = [
      ...new Set(
        r.datasets.flatMap((d) =>
          d.sources.map((s) =>
            keyOf({ connection_id: s.connection_id, schema: s.schema || "", table: s.table })
          )
        )
      ),
    ];
    setSpecText(text);
    setSavedSpecText(text);
    setQuestion(r.question || "");
    setPicked(keys);
    setDraft(null);
    setPayload(null);
    setOpenId(r.id);
    setOpenName(r.name);
    setTab("board");
    keys.forEach(ensureColumns);
    runId.current += 1; // 让可能在飞的草稿/渲染响应作废

    // 连接被删过就别去取数：后端只会回一句"找不到连接"，
    // 用户看不出该回去补哪张表
    const need = [...new Set(r.datasets.flatMap((d) => d.sources.map((s) => s.connection_id)))];
    const gone = need.filter((id) => !configs.some((c) => c.id === id));
    if (gone.length) {
      setError({
        title: "报表需要的连接不在本机",
        detail: `这张报表用到的连接已不存在：${gone.join("、")}。\n请在左侧重新选表，或改规格 JSON 里的 connection_id 后再取数。`,
      });
      msgApi.warning("缺少连接，已载入规格但未取数");
      return;
    }
    await runRender(r.view, r.datasets);
  };

  const onDeleteReport = async (r: SavedReport) => {
    try {
      await deleteReport(r.id);
      // 删的正是当前打开的那条：清掉 openId，否则下一次"保存"会把已删条目复活
      if (openId === r.id) {
        setOpenId("");
        setOpenName("");
        setSavedSpecText("");
      }
      await refreshReports();
      msgApi.success(`已从报表簿移除「${r.name}」`);
    } catch (e) {
      msgApi.error(`删除失败：${e}`);
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
        {/* 目录现状要说得出口：哪几张表真带着字段进了目录、哪几张没进去、为什么。
            以前连接被删的表会被静默滤掉，用户看到"已生成"却不知道模型少看了两张表。 */}
        {colBusy.length > 0 && (
          <Text type="secondary" style={{ fontSize: 12, display: "block" }}>
            正在读取 {colBusy.length} 张表的列清单…
          </Text>
        )}
        {catalog.map((t) => (
          <Tooltip key={keyOf(t)} title={`${t.columns.join(", ")}\n方言：${t.database_type}`}>
            <Tag color="blue" style={{ margin: "4px 4px 0 0" }}>
              {t.connection_name}/{t.table} · {t.columns.length} 列
            </Tag>
          </Tooltip>
        ))}
        {unusable
          .filter((u) => !colBusy.includes(u.key))
          .map((u) => (
            <Tooltip key={u.key} title={`${u.conn}：${u.why}`}>
              <Tag color="red" style={{ margin: "4px 4px 0 0" }}>
                {u.table} 进不了目录
              </Tag>
            </Tooltip>
          ))}
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
            onClose={() => setError(null)}
            style={{ marginBottom: 12 }}
            title={error.title}
            description={
              <div style={{ whiteSpace: "pre-wrap", fontFamily: "monospace", fontSize: 12 }}>
                {error.detail}
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
          <Tooltip
            title={
              openId
                ? `当前稿对应报表簿里的「${openName}」。存的是规格（数据集 + 视图），不是这一次跑出来的数字。`
                : "存的是规格（数据集 + 视图），不是这一次跑出来的数字"
            }
          >
            {/* 名字放 tooltip 而不是按钮上：报表名是用户手打的，写进文案里会把这一排按钮挤没 */}
            <Button icon={<SaveOutlined />} disabled={!specReady} onClick={openSaveDialog}>
              {openId ? (dirty ? "更新报表（改过）" : "已存入报表") : "存入报表簿"}
            </Button>
          </Tooltip>
          {payload && (
            <Button
              icon={<ReloadOutlined />}
              onClick={() => {
                setPayload(null);
                setDraft(null);
                setSpecText("");
                setQuestion("");
                setOpenId("");
                setOpenName("");
                setSavedSpecText("");
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
          activeKey={tab}
          onChange={setTab}
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
                        <Text>
                          还没有草稿。左侧选表并描述需求，切到「规格 JSON」手搓，或者从「报表簿
                          {reports.length ? `（${reports.length} 张）` : ""}」打开上次存下的报表。
                        </Text>
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
              key: "book",
              label: reportsError ? "报表簿（异常）" : `报表簿 ${reports.length}`,
              children: reportsError ? (
                // 读失败绝不能退化成"报表簿还是空的"：那会让用户以为存过的东西没了
                <Alert
                  type="error"
                  showIcon
                  title="报表簿读不出来"
                  description={
                    <Space orientation="vertical" style={{ width: "100%" }}>
                      <div
                        style={{ whiteSpace: "pre-wrap", fontFamily: "monospace", fontSize: 12 }}
                      >
                        {reportsError}
                      </div>
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        落盘文件是 reports.json（带校验和）。真损坏了就删掉这个文件重新开始，
                        存过的报表规格不会自动找回。
                      </Text>
                      <Button size="small" icon={<ReloadOutlined />} onClick={refreshReports}>
                        重试
                      </Button>
                    </Space>
                  }
                />
              ) : reportsLoading && reports.length === 0 ? (
                <Spin style={{ display: "block", margin: "60px auto" }} />
              ) : reports.length === 0 ? (
                <Empty
                  description="报表簿还是空的。跑通一稿后点「存入报表簿」，下次直接点开就能重跑。"
                  style={{ marginTop: 60 }}
                />
              ) : (
                <List
                  dataSource={reports}
                  renderItem={(r) => {
                    const conns = [
                      ...new Set(r.datasets.flatMap((d) => d.sources.map((s) => s.connection_id))),
                    ];
                    const named = conns.map((id) => configs.find((c) => c.id === id)?.name || id);
                    return (
                      <List.Item
                        key={r.id}
                        actions={[
                          <Button
                            key="open"
                            size="small"
                            type="primary"
                            ghost
                            icon={<FolderOpenOutlined />}
                            onClick={() => onOpenReport(r)}
                          >
                            打开并取数
                          </Button>,
                          <Popconfirm
                            key="del"
                            title={`移除「${r.name}」？`}
                            description="只删本地这一条报表规格，不会动库里的数据。"
                            okText="移除"
                            // 应用没有挂 zh_CN 的 ConfigProvider，不显式给就是英文 Cancel
                            cancelText="取消"
                            okButtonProps={{ danger: true }}
                            onConfirm={() => onDeleteReport(r)}
                          >
                            <Button size="small" danger type="text" icon={<DeleteOutlined />} />
                          </Popconfirm>,
                        ]}
                      >
                        <List.Item.Meta
                          title={
                            <Space size={6} wrap>
                              <Text strong>{r.name}</Text>
                              {r.id === openId && <Tag color="processing">当前</Tag>}
                              <Tag color={conns.length > 1 ? "purple" : "default"}>
                                {conns.length > 1 ? `跨 ${conns.length} 库` : `单库`}
                              </Tag>
                              <Tag>{r.datasets.length} 数据集</Tag>
                              <Tag>{r.view.widgets.length} 组件</Tag>
                              <Text type="secondary" style={{ fontSize: 12 }}>
                                {new Date(r.updated_at * 1000).toLocaleString()}
                              </Text>
                            </Space>
                          }
                          description={
                            <Space orientation="vertical" size={2} style={{ width: "100%" }}>
                              {r.description && <Text>{r.description}</Text>}
                              <Text type="secondary" style={{ fontSize: 12 }} ellipsis>
                                连接：{named.join(" + ")}
                              </Text>
                              {r.question && (
                                <Text type="secondary" style={{ fontSize: 12 }} ellipsis>
                                  问题：{r.question}
                                </Text>
                              )}
                            </Space>
                          }
                        />
                      </List.Item>
                    );
                  }}
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

      <Modal
        open={saveOpen}
        title={openId ? `保存到报表簿 · ${openName}` : "存入报表簿"}
        width={520}
        onCancel={() => setSaveOpen(false)}
        footer={[
          <Button key="cancel" onClick={() => setSaveOpen(false)}>
            取消
          </Button>,
          // 已经在报表簿里一条上：给一条不改原稿的岔路，改名不再是"覆盖旧报表"
          ...(openId
            ? [
                <Button key="fork" loading={saving} onClick={() => onSave(true)}>
                  另存为新报表
                </Button>,
              ]
            : []),
          <Button key="ok" type="primary" loading={saving} onClick={() => onSave(false)}>
            保存
          </Button>,
        ]}
      >
        <Space orientation="vertical" style={{ width: "100%" }}>
          <Input
            autoFocus
            value={saveName}
            onChange={(e) => setSaveName(e.target.value)}
            placeholder="报表名称，例：月度 GMV（商城库 × 客户库）"
            onPressEnter={() => onSave(false)}
          />
          <TextArea
            rows={2}
            value={saveDesc}
            onChange={(e) => setSaveDesc(e.target.value)}
            placeholder="备注（可选）：口径、过滤条件为什么这么写"
          />
          <Text type="secondary" style={{ fontSize: 12 }}>
            存的是规格：数据集定义与视图布局。每次打开仍会重新取数，看到的永远是库里当下的数字；
            连接被删过会在载入时点出来，不会静默给一张空看板。
          </Text>
        </Space>
      </Modal>
    </div>
  );
}

export default ReportWorkbench;
