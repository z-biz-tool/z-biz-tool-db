// 两版报表规格的差异。
//
// 起草会整份覆盖编辑器里的规格，而"在上一版基础上改"只是对模型的一句请求：
// 本机校验只查引用合法性（表、列、数据集 id 挂得对不对），模型少写两个组件照样过。
// 所以这里把覆盖前后的两份对一遍，让"丢了什么"当场看得见，而不是回头发现图少了一半。

import type { DatasetSpec, ViewSpec, WidgetSpec } from "./types";

export interface SpecSide {
  datasets: DatasetSpec[];
  view: ViewSpec;
}

export interface Side {
  kept: number;
  added: string[];
  dropped: string[];
  changed: string[];
  from: number;
  to: number;
}

export interface SpecDiff {
  ds: Side;
  w: Side;
}

/** 键序不同不算改过：手搓的 JSON 与 serde 出来的字段顺序没约定 */
function stable(v: unknown): string {
  if (Array.isArray(v)) return `[${v.map(stable).join(",")}]`;
  if (v && typeof v === "object")
    return `{${Object.keys(v)
      .sort()
      .map((k) => `${k}:${stable((v as Record<string, unknown>)[k])}`)
      .join(",")}}`;
  return JSON.stringify(v) ?? "";
}

/** 按 id 比对两份同型清单。id 换过名会算成"去掉 + 新增"，这比硬凑"改了什么"诚实 */
function countBy<T extends { id: string }>(prev: T[], next: T[], label: (x: T) => string): Side {
  const before = new Map(prev.map((x) => [x.id, x]));
  const after = new Map(next.map((x) => [x.id, x]));
  const s: Side = { kept: 0, added: [], dropped: [], changed: [], from: prev.length, to: next.length };
  for (const [id, x] of after) {
    const p = before.get(id);
    if (!p) s.added.push(label(x));
    else if (stable(p) !== stable(x)) s.changed.push(label(x));
    else s.kept += 1;
  }
  for (const [id, x] of before) if (!after.has(id)) s.dropped.push(label(x));
  return s;
}

export const diffSpec = (from: SpecSide, to: SpecSide): SpecDiff => ({
  ds: countBy(from.datasets, to.datasets, (d) => (d.name ? `${d.id}（${d.name}）` : d.id)),
  // 模型有时不给组件起名，只报 id 用户认不出是哪张图，补上类型
  w: countBy(from.view.widgets, to.view.widgets, (x: WidgetSpec) => x.title || `${x.type} · ${x.id}`),
});

/** 一栏一句，只说有变化的那几类；"布局也重排了"这类没量到的不硬凑 */
export const describeDiff = (d: SpecDiff): string[] => {
  const lines: string[] = [];
  const part = (s: Side, unit: string) => {
    const bits: string[] = [];
    if (s.kept) bits.push(`${s.kept} 个原样`);
    if (s.changed.length) bits.push(`改了 ${s.changed.join("、")}`);
    if (s.added.length) bits.push(`新增 ${s.added.join("、")}`);
    if (s.dropped.length) bits.push(`去掉 ${s.dropped.join("、")}`);
    return bits.length ? `${unit}：${bits.join("，")}` : "";
  };
  const ds = part(d.ds, "数据集");
  const w = part(d.w, "组件");
  const moved =
    d.ds.changed.length + d.ds.added.length + d.ds.dropped.length +
    d.w.changed.length + d.w.added.length + d.w.dropped.length;
  // 只有"原样"两字时别装作有变化：模型把上一版整份重抄一遍也要说清
  if (!moved) lines.push(`两版一字不差：模型把上一版原样又写了一遍（数据集 ${d.ds.to} 个、组件 ${d.w.to} 个）`);
  else {
    if (ds) lines.push(ds);
    if (w) lines.push(w);
  }
  lines.push("对照的是覆盖前后的两份；组件位置（布局）与报表名不在比对范围内");
  return lines;
};
