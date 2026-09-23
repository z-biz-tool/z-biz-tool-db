// 单个图表卡片：把后端 ChartData 画出来。
//
// 后端已经把 categories / series / rows / value 摊平好了，前端只负责画，
// 不再二次聚合——两边各算一遍统计口径，迟早算出两个数。

import { Alert, Button, Card, Empty, Statistic, Table, Tag, Tooltip, Typography } from "antd";
import { WarningOutlined, DownloadOutlined } from "@ant-design/icons";
import { csvDoc, downloadCsv, stampName } from "./csv";
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

/** 这张图画出来的那些数 → CSV 的行列。图上是什么就导什么，不做二次加工。 */
export function chartCsv(chart: ChartData): { headers: string[]; rows: string[][] } {
  if (chart.kind === "TABLE") {
    const headers = chart.columns.length
      ? chart.columns
      : (chart.rows[0] || []).map((_, i) => `列${i + 1}`);
    return { headers, rows: chart.rows.map((r) => r.map((v) => scalarText(v))) };
  }
  if (chart.kind === "KPI") {
    return {
      headers: ["指标", "值"],
      rows: [[chart.title || chart.widget, scalarText(chart.value)]],
    };
  }
  // 柱/折/饼：x 轴是 categories，每条 series 一列。不复用 toSeriesTable——
  // 那份是给 recharts 画图用的，键名（__name）漏进表头就是给人看的脏数据
  const headers = ["类别", ...chart.series.map((s, i) => s.name || `系列${i + 1}`)];
  const rows = chart.categories.map((c, i) => [
    scalarText(c),
    ...chart.series.map((s) => scalarText(s.values[i] ?? null)),
  ]);
  return { headers, rows };
}

export function ChartCard({
  chart,
  datasetName,
  height,
  partial,
  onNotice,
}: {
  chart: ChartData;
  /** 这张图吃的是哪个数据集，出错时至少要能对上号 */
  datasetName?: string;
  height: number;
  /** 上游数据集撞上取数上限：导出的也只是已取回那部分，得说出来 */
  partial?: boolean;
  onNotice?: (kind: "success" | "warning", text: string) => void;
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
        <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
        <Tooltip title="导出这张图为 CSV。导的是图上这些数（已聚合、已截断的口径），不是库里的原始行。">
          <Button
            size="small"
            type="text"
            icon={<DownloadOutlined />}
            onClick={() => {
              const { headers, rows } = chartCsv(chart);
              if (!headers.length && !rows.length) {
                onNotice?.("warning", "这张图没有可导出的数");
                return;
              }
              downloadCsv(stampName(`报表_${chart.title || chart.widget}`), csvDoc(headers, rows));
              if (partial) {
                onNotice?.(
                  "warning",
                  "上游数据集撞上取数上限：这张图和刚导出的 CSV 只覆盖已取回的部分"
                );
              } else {
                onNotice?.("success", "已导出这张图的数");
              }
            }}
          />
        </Tooltip>
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
        </span>
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
