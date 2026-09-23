// 报表看板：按后端回传的 layout 在 12 栅格上绝对定位，并把"这次结果可不可信"
// 说清楚（截断、耗时、行数、生成的 SQL）。布局只读后端结果，前端不再排一次，
// 否则同一张看板会有两个版本。

import { Alert, Button, Card, Collapse, Space, Table, Tag, Tooltip, Typography } from "antd";
import { DatabaseOutlined, FieldTimeOutlined } from "@ant-design/icons";
import { ChartCard } from "./ChartCard";
import type { SqlPreview, ViewPayload } from "./types";

const { Text } = Typography;

/** 与后端 view::GRID_COLUMNS 同值 */
const GRID_COLUMNS = 12;
/** 一个栅格行的高度（px），布局里的 h 乘上它才是卡片高度 */
const ROW_UNIT = 44;

export function SqlList({ sqls }: { sqls: SqlPreview[] }) {
  if (sqls.length === 0) return <Text type="secondary">这次没有下推任何 SQL</Text>;
  return (
    <Collapse
      size="small"
      items={sqls.map((s) => ({
        key: `${s.dataset}/${s.alias}`,
        label: (
          <Space size={6} wrap>
            <Tag color="blue">{s.dataset}</Tag>
            <Text code>{s.alias}</Text>
            <Text type="secondary" style={{ fontSize: 12 }}>
              {s.connection_name || s.connection_id} · {s.database_type}
            </Text>
          </Space>
        ),
        children: (
          <Typography.Paragraph
            copyable
            style={{
              marginBottom: 0,
              whiteSpace: "pre-wrap",
              fontFamily: "monospace",
              fontSize: 12,
            }}
          >
            {s.sql}
          </Typography.Paragraph>
        ),
      }))}
    />
  );
}

export function DatasetStats({ payload }: { payload: ViewPayload }) {
  return (
    <Table
      size="small"
      pagination={false}
      dataSource={payload.datasets.map((d) => ({ ...d, key: d.id }))}
      columns={[
        {
          title: "数据集",
          dataIndex: "name",
          render: (_v, r: any) => (
            <Space size={6}>
              <DatabaseOutlined />
              <Text strong>{r.name}</Text>
              <Text code style={{ fontSize: 11 }}>
                {r.id}
              </Text>
            </Space>
          ),
        },
        { title: "输出列", dataIndex: "columns", render: (c: string[]) => c.join(", ") || "—" },
        { title: "行数", dataIndex: "rows", width: 90, align: "right" },
        {
          title: "取数",
          dataIndex: "elapsed_ms",
          width: 100,
          align: "right",
          render: (ms: number) => (
            <Tooltip title="含跨库拉数与本机算子链">
              <span>
                <FieldTimeOutlined /> {ms} ms
              </span>
            </Tooltip>
          ),
        },
        {
          title: "完整性",
          dataIndex: "partial",
          width: 160,
          render: (partial: boolean, r: any) =>
            partial ? (
              <Tag color="orange">截断：{r.truncated.join(", ")}</Tag>
            ) : (
              <Tag color="green">完整</Tag>
            ),
        },
      ]}
    />
  );
}

export function ReportBoard({
  payload,
  /** 组件 id → 数据集名。ChartData 本身不带数据集，只能由持有 spec 的调用方给 */
  datasetNameOf,
  /** 缺数那张卡上的就地重试：重跑的是整张报表的取数，不是只补那几张，文案别写歪 */
  onRetry,
  retryBusy,
}: {
  payload: ViewPayload;
  datasetNameOf?: (widgetId: string) => string | undefined;
  onRetry?: () => void;
  retryBusy?: boolean;
}) {
  const chartById = new Map(payload.charts.map((c) => [c.widget, c]));
  const placed = new Set(payload.layout.map((l) => l.widget));
  const orphans = payload.charts.filter((c) => !placed.has(c.widget));
  const bottom = payload.layout.reduce((m, l) => Math.max(m, l.y + l.h), 0);
  // 每个数据集都可能有自己的截断源，别名（o、u）在跨库报表里会重复——
  // 只列别名等于没告诉用户该回去调哪个数据集的 max_rows。
  const truncated = payload.datasets
    .filter((d) => d.partial)
    .map(
      (d) => `${d.name}（${d.id}）${d.truncated.length ? ` 的源 ${d.truncated.join("、")}` : ""}`
    );
  const failed = payload.failed;

  return (
    <Space orientation="vertical" size={12} style={{ width: "100%" }}>
      {/* 跨库报表里一条连接断了不该白屏，但少画的图必须点名说，
          否则用户会把"三张图变一张"当成数据本来就长这样。 */}
      {failed.length > 0 && (
        <Alert
          type="warning"
          showIcon
          title={`${failed.length} 个数据集没取到数，${failed.reduce((n, f) => n + f.widgets.length, 0)} 个组件没画出来`}
          description={
            <Space orientation="vertical" size={2} style={{ fontSize: 12 }}>
              {failed.map((f) => (
                <div key={f.id}>
                  · {f.name}（{f.id}）：{f.error}
                  {f.widgets.length > 0 ? ` —— 受影响组件 ${f.widgets.join("、")}` : ""}
                </div>
              ))}
              <div style={{ opacity: 0.75 }}>
                {onRetry
                  ? "其余数据集已照常取数；补上连接或改好表名后，用右边这个「再取一次数」重跑整张报表的取数。"
                  : "其余数据集已照常取数；补上连接或改好表名后再点一次「取数并渲染」。"}
              </div>
            </Space>
          }
          action={
            onRetry ? (
              <Button size="small" loading={retryBusy} onClick={onRetry}>
                再取一次数
              </Button>
            ) : null
          }
        />
      )}
      {payload.partial && (
        <Alert
          type="warning"
          showIcon
          title="结果不完整"
          description={`以下数据集撞上 max_rows 上限，聚合与排序都只覆盖已取回的部分：${truncated.join("；")}。调大对应数据集的 max_rows 再跑一次。`}
        />
      )}
      <div style={{ position: "relative", height: (bottom + orphans.length * 6) * ROW_UNIT }}>
        {payload.layout.map((l) => {
          const chart = chartById.get(l.widget);
          if (!chart) return null;
          return (
            <div
              key={l.widget}
              style={{
                position: "absolute",
                left: `${(l.x / GRID_COLUMNS) * 100}%`,
                top: l.y * ROW_UNIT,
                width: `${(l.w / GRID_COLUMNS) * 100}%`,
                height: l.h * ROW_UNIT,
                padding: 4,
                boxSizing: "border-box",
              }}
            >
              <ChartCard
                chart={chart}
                datasetName={datasetNameOf?.(l.widget)}
                height={l.h * ROW_UNIT}
              />
            </div>
          );
        })}
        {/* 后端没给布局的组件（版本偏旧或手工 spec）也画出来，静默丢图比丑更糟 */}
        {orphans.map((c, i) => (
          <div
            key={c.widget}
            style={{
              position: "absolute",
              left: 0,
              top: (bottom + i * 6) * ROW_UNIT,
              width: "100%",
              height: 6 * ROW_UNIT,
              padding: 4,
              boxSizing: "border-box",
            }}
          >
            <ChartCard chart={c} datasetName={datasetNameOf?.(c.widget)} height={6 * ROW_UNIT} />
          </div>
        ))}
      </div>
      <Card size="small" title="数据集执行概况" style={{ borderRadius: 12 }}>
        <DatasetStats payload={payload} />
      </Card>
    </Space>
  );
}

export default ReportBoard;
