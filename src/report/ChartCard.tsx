// 单个图表卡片：把后端 ChartData 画出来。
//
// 后端已经把 categories / series / rows / value 摊平好了，前端只负责画，
// 不再二次聚合——两边各算一遍统计口径，迟早算出两个数。

import { Alert, Card, Empty, Statistic, Table, Tag, Tooltip, Typography } from "antd";
import { WarningOutlined } from "@ant-design/icons";
import {
  Bar,
  BarChart,
  CartesianGrid,
  Cell,
  Legend,
  Line,
  LineChart,
  Pie,
  PieChart,
  ResponsiveContainer,
  Tooltip as ChartTooltip,
  XAxis,
  YAxis,
} from "recharts";
import type { ChartData, Scalar } from "./types";

const { Text } = Typography;

export const CHART_COLORS = [
  "#667eea",
  "#52c41a",
  "#faad14",
  "#eb2f96",
  "#13c2c2",
  "#722ed1",
  "#fa541c",
  "#1890ff",
];

/** tooltip 回调可能给出数组/对象，这里只认标量，其余按 NULL 显示 */
function asScalar(v: unknown): Scalar {
  return typeof v === "number" || typeof v === "string" || typeof v === "boolean" ? v : null;
}

function asNumber(v: Scalar): number | null {
  if (typeof v === "number") return Number.isFinite(v) ? v : null;
  if (typeof v === "boolean") return v ? 1 : 0;
  if (typeof v === "string") {
    const n = Number(v);
    return v.trim() !== "" && Number.isFinite(n) ? n : null;
  }
  return null;
}

/** 展示用文本：数值带千分位，NULL 明确写出来而不是留白 */
export function scalarText(v: Scalar): string {
  if (v === null || v === undefined) return "NULL";
  if (typeof v === "number") {
    return Number.isInteger(v)
      ? v.toLocaleString("zh-CN")
      : v.toLocaleString("zh-CN", { minimumFractionDigits: 2, maximumFractionDigits: 4 });
  }
  return String(v);
}

/** 折线/柱状：categories 做 x，每条 series 一个数值键 */
function toSeriesTable(chart: ChartData): Array<Record<string, number | string | null>> {
  return chart.categories.map((c, i) => {
    const row: Record<string, number | string | null> = { __name: c };
    for (const s of chart.series) row[s.name] = asNumber(s.values[i] ?? null);
    return row;
  });
}

function Cartesian({ chart }: { chart: ChartData }) {
  const data = toSeriesTable(chart);
  const bars = chart.kind === "BAR";
  const Chart = bars ? BarChart : LineChart;
  return (
    <ResponsiveContainer width="100%" height="100%">
      <Chart data={data} margin={{ top: 8, right: 16, bottom: 8, left: 0 }}>
        <CartesianGrid strokeDasharray="3 3" vertical={false} />
        <XAxis dataKey="__name" tick={{ fontSize: 12 }} interval="preserveStartEnd" />
        <YAxis tick={{ fontSize: 12 }} width={72} tickFormatter={(v) => scalarText(Number(v))} />
        <ChartTooltip formatter={(v) => scalarText(asScalar(v))} />
        {chart.series.length > 1 && <Legend wrapperStyle={{ fontSize: 12 }} />}
        {chart.series.map((s, i) =>
          bars ? (
            // 一律并排不堆叠：后端没声明 stack，UI 自己叠会把总量画成看起来像单系列
            <Bar
              key={s.name}
              dataKey={s.name}
              fill={CHART_COLORS[i % CHART_COLORS.length]}
              isAnimationActive={false}
            />
          ) : (
            <Line
              key={s.name}
              type="monotone"
              dataKey={s.name}
              stroke={CHART_COLORS[i % CHART_COLORS.length]}
              strokeWidth={2}
              dot={{ r: 2 }}
              isAnimationActive={false}
            />
          )
        )}
      </Chart>
    </ResponsiveContainer>
  );
}

function PieBlock({ chart }: { chart: ChartData }) {
  const values = chart.series[0]?.values ?? [];
  const data = chart.categories.map((c, i) => ({
    name: c,
    value: asNumber(values[i] ?? null) ?? 0,
  }));
  return (
    <ResponsiveContainer width="100%" height="100%">
      <PieChart>
        <Pie
          data={data}
          dataKey="value"
          nameKey="name"
          outerRadius="72%"
          label={(d) => d.name ?? ""}
        >
          {data.map((_, i) => (
            <Cell key={i} fill={CHART_COLORS[i % CHART_COLORS.length]} />
          ))}
        </Pie>
        <ChartTooltip formatter={(v) => scalarText(asScalar(v))} />
        <Legend wrapperStyle={{ fontSize: 12 }} />
      </PieChart>
    </ResponsiveContainer>
  );
}

function TableBlock({ chart }: { chart: ChartData }) {
  const columns = chart.columns.map((c, ord) => ({
    title: c,
    dataIndex: `c${ord}`,
    key: `c${ord}`,
    ellipsis: true,
    render: (v: Scalar) =>
      v === null || v === undefined ? (
        <Text type="secondary">NULL</Text>
      ) : (
        <span style={{ fontVariantNumeric: "tabular-nums" }}>{scalarText(v)}</span>
      ),
  }));
  const dataSource = chart.rows.map((row, i) => {
    const obj: Record<string, Scalar> = { key: i } as Record<string, Scalar>;
    row.forEach((v, ord) => (obj[`c${ord}`] = v));
    return obj;
  });
  return (
    <Table
      size="small"
      columns={columns}
      dataSource={dataSource}
      pagination={chart.rows.length > 20 ? { pageSize: 20, size: "small" } : false}
      scroll={{ x: "max-content" }}
    />
  );
}

export function ChartCard({
  chart,
  datasetName,
  height,
}: {
  chart: ChartData;
  /** 这张图吃的是哪个数据集，出错时至少要能对上号 */
  datasetName?: string;
  height: number;
}) {
  const empty =
    chart.kind === "TABLE"
      ? chart.rows.length === 0
      : chart.kind === "KPI"
        ? chart.value === null
        : chart.categories.length === 0;

  return (
    <Card
      size="small"
      style={{ height: "100%", borderRadius: 12, overflow: "hidden" }}
      title={
        <span>
          {chart.title || chart.widget}{" "}
          {datasetName && <Tag style={{ marginLeft: 6 }}>{datasetName}</Tag>}
        </span>
      }
      extra={
        <Tooltip title={chart.warnings.join("\n")}>
          <Text type="secondary" style={{ fontSize: 11 }}>
            {chart.warnings.length > 0 && (
              <WarningOutlined style={{ color: "#faad14", marginRight: 4 }} />
            )}
            {chart.kind === "TABLE" || chart.kind === "KPI"
              ? `${chart.row_count} 行`
              : `${chart.categories.length} 类`}
          </Text>
        </Tooltip>
      }
    >
      {chart.warnings.length > 0 && (
        <Alert
          type="warning"
          showIcon
          style={{ marginBottom: 8 }}
          title={chart.warnings[0]}
          description={
            chart.warnings.length > 1 ? `另有 ${chart.warnings.length - 1} 条告警` : undefined
          }
        />
      )}
      {empty ? (
        <Empty
          image={Empty.PRESENTED_IMAGE_SIMPLE}
          description="没有命中数据（聚合后为空或过滤条件太严）"
          style={{ marginTop: height / 3 }}
        />
      ) : chart.kind === "KPI" ? (
        <div style={{ height: "100%", display: "flex", alignItems: "center" }}>
          <Statistic value={scalarText(chart.value)} styles={{ content: { fontSize: 30 } }} />
        </div>
      ) : chart.kind === "TABLE" ? (
        <div style={{ height: "100%", overflow: "auto" }}>
          <TableBlock chart={chart} />
        </div>
      ) : chart.kind === "PIE" ? (
        <PieBlock chart={chart} />
      ) : (
        <Cartesian chart={chart} />
      )}
    </Card>
  );
}

export default ChartCard;
