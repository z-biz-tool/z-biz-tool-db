// CSV 出口只有一份：SQL 工作台的"导出结果"和看板上的"导出这张图"必须走同一套
// 转义与公式防护。两边各写一遍迟早漂——漂出去的那一份就是拿 CSV 去对账时少的引号。

/** 一个单元格：公式注入要挡，含逗号/引号要包起来 */
export const csvCell = (raw: string): string => {
  // 以 = + - @ 制表符 换行 开头的值会被 Excel 当公式执行，加前缀单引号废掉
  if (/^[=+\-@\t\n]/.test(raw)) return "'" + raw;
  if (raw.includes(",") || raw.includes('"')) return '"' + raw.replace(/"/g, '""') + '"';
  return raw;
};

export const csvRow = (cells: string[]): string => cells.map(csvCell).join(",");

export const csvDoc = (headers: string[], rows: string[][]): string =>
  [csvRow(headers), ...rows.map(csvRow)].join("\n");

/** 文件名要过一遍：图表标题来自模型或用户手改，里面一个 `/` 就变成"下到别的目录"，
 *  `..`、`:`、`"*?<>|\` 这些在 Windows 上要么失败要么被改名，控制字符更是直接截断。
 *  这里不追求好看，只保证"叫什么就是什么"。 */
export const safeFileName = (raw: string, fallback = "报表"): string => {
  const cleaned = raw
    // eslint-disable-next-line no-control-regex
    .replace(/[\u0000-\u001f]/g, " ")
    .replace(/[\\/:*?"'<>|]+/g, "_")
    .replace(/\.{2,}/g, ".")
    .replace(/^[.\s]+/, "")
    .replace(/[\s.]+$/, "")
    .replace(/\s+/g, " ")
    .slice(0, 60)
    .trim();
  return cleaned || fallback;
};

/** 浏览器下载：桌面端与 dev 都在 webview 里，一条 a[download] 就够 */
export const downloadCsv = (fileName: string, text: string): void => {
  const url = URL.createObjectURL(new Blob([text], { type: "text/csv;charset=utf-8;" }));
  const a = document.createElement("a");
  a.href = url;
  // 扩展名单独拼：safeFileName 会把结尾的点吃掉，不然 "名字." 在 Windows 上是个雷
  const base = fileName.replace(/\.csv$/, "");
  a.download = `${safeFileName(base)}.csv`;
  a.click();
  URL.revokeObjectURL(url);
};

export const stampName = (prefix: string): string =>
  `${prefix}_${new Date().toISOString().slice(0, 10)}.csv`;
