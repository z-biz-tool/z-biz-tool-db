// 探针专用：把 window.__TAURI_INTERNALS__.invoke 换成固定契约，
// 让报表簿链路能在浏览器里跑完整状态机。契约数据来自真实 Rust 测试产物。
import fixture from "./fixture.json";

declare global {
  interface Window {
    __TAURI_INTERNALS__: any;
    __PROBE_CALLS: any[];
    __PROBE_SAVED: any[];
    __PROBE_DELETE: string[];
    __PROBE_FLAG: (k: string) => boolean;
    __PROBE_ARG: (k: string) => string;
  }
}

const q = new URLSearchParams(window.location.search);
window.__PROBE_FLAG = (k: string) => q.get(k) === "1";
// 定向注入：?descfail=orders 只让 orders 这张表的列清单读不出来
window.__PROBE_ARG = (k: string) => q.get(k) || "";

window.__PROBE_CALLS = [];
window.__PROBE_SAVED = [];
window.__PROBE_DELETE = [];

// 报表簿的"磁盘"：内存里模拟，可被保存/删除改动
let reports: any[] = JSON.parse(JSON.stringify((fixture as any).reports));
// descfailonce 已经失败过哪几张表
const failedOnce = new Set<string>();

function push(cmd: string, args: any) {
  window.__PROBE_CALLS.push({ cmd, args });
}

function fail(msg: string) {
  return Promise.reject(msg);
}

// 镜像 ai_sql.rs 的 generate()：产出必须只用目录里真实存在的列，
// 否则按后端同一条拒因打回——探针里模型也不能编字段。
function mirrorDraft(question: string, catalog: any[]): any {
  const tables: string[] = catalog.map((t) => t.table);
  const byName: Record<string, string[]> = {};
  for (const t of catalog) byName[t.table] = t.columns || [];
  const picked = tables.filter((t) => question.toLowerCase().includes(t.toLowerCase()));
  const used = picked.length ? picked : [tables[0]];
  const cols = used.flatMap((t) => (byName[t] || []).slice(0, 2).map((c: string) => `${t}.${c}`));
  const sql = `SELECT ${cols.join(", ")} FROM ${used[0]}${
    used.length > 1 ? " JOIN " + used[1] + " ON " + used[0] + ".id = " + used[1] + ".id" : ""
  }`;
  const bad = (sql.match(/([a-z_][a-z_0-9]*)\.([a-z_][a-z_0-9]*)/gi) || []).find((ref) => {
    const [t, c] = ref.split(".");
    return byName[t] !== undefined && !byName[t].includes(c);
  });
  if (bad) {
    const t = bad.split(".")[0];
    return {
      reject: `重试 2 次后仍未通过本机校验：SQL 里的字段 ${bad} 不存在（${t} 有：${byName[t].join(", ")}）`,
    };
  }
  const dialects = Array.from(new Set(catalog.map((t) => String(t.database_type)).sort()));
  const warnings: string[] = [];
  if (catalog.length > 1)
    warnings.push(`本次目录跨 ${dialects.length} 种方言，join 只在各自源里成立`);
  return {
    draft: {
      sql,
      tables: used,
      dialect: dialects.length ? dialects.join("/") : "标准",
      warnings,
      repairs: question.length % 3,
    },
  };
}

/** 探查结果：名字来自 fixture.columns，类型来自 fixture.column_types。
 *  column_types 里没有的列必须回空串——真实 sqlite 里不写类型的列就是这样，
 *  探针要连这条腿一起跑，不能只测"每个列都有类型"的理想情况。 */
function described(table: string): { name: string; data_type: string }[] {
  const names: string[] = (fixture as any).columns[table] || [];
  const types: Record<string, string> = (fixture as any).column_types?.[table] || {};
  return names.map((name) => ({ name, data_type: types[name] || "" }));
}

// 与 Rust ai.rs family_of 同口径：只看类型名开头的那个词，认不出的保持沉默。
// fixture 里 orders.user_id 是 int、users.id 是 varchar(64)——就是跨库最常见的
// 那一对"值相同、桶不同"的连接键，探针要能在浏览器里看见它被拦下来。
const NUM_TY = [
  "int", "integer", "tinyint", "smallint", "mediumint", "bigint", "int2", "int4", "int8",
  "serial", "bigserial", "smallserial", "float", "double", "real", "decimal", "dec",
  "numeric", "fixed", "money", "smallmoney", "bool", "boolean",
];
const TXT_TY = [
  "char", "varchar", "nchar", "nvarchar", "text", "tinytext", "mediumtext", "longtext",
  "character", "citext", "string", "name", "clob", "uuid", "json", "jsonb", "enum", "set",
  "date", "datetime", "smalldatetime", "time", "timestamp", "timestamptz", "year",
];
const BIN_TY = [
  "binary", "varbinary", "blob", "tinyblob", "mediumblob", "longblob", "bytea", "geometry",
  "geography", "image", "vector",
];
function typeHead(ty: string): string {
  return (String(ty || "").trim().toLowerCase().match(/^[a-z]+/) || [])[0] || "";
}
function familyOf(ty: string): string {
  const head = typeHead(ty);
  if (NUM_TY.includes(head)) return "num";
  if (TXT_TY.includes(head)) return "text";
  if (BIN_TY.includes(head)) return "bin";
  return "?";
}
/** 永远写不成整数键的文本类型：这一档仍然是必然空表 */
const NEVER_INT_TY = [
  "date", "datetime", "smalldatetime", "time", "timestamp", "timestamptz", "uuid", "json", "jsonb",
];

/** 镜像 ai.rs join_key_warnings：连接键跨族按三档措辞（同族保持沉默）。
 *  左侧累计列的命名规则与执行期 plan_join_columns 一致（重名依次加 _2/_3）。 */
function joinKeyWarnings(specs: any[], catalog: any[]): string[] {
  const out: string[] = [];
  for (const ds of specs) {
    const byAlias: Record<string, { cols: string[]; ty: (c: string) => string }> = {};
    for (const s of ds.sources || []) {
      const hit = catalog.find(
        (t: any) => t.connection_id === s.connection_id && t.table === s.table
      );
      byAlias[s.alias] = {
        cols: (hit && hit.columns) || [],
        ty: (c: string) => String((hit && hit.column_types && hit.column_types[c]) || ""),
      };
    }
    const base = byAlias[ds.base];
    if (!base) continue;
    let acc = base.cols.map((c) => ({ name: c, label: `${ds.base}.${c}`, ty: base.ty(c) }));
    for (const j of ds.joins || []) {
      const right = byAlias[j.source];
      for (const p of j.on || []) {
        const l = acc.find((a) => a.name.toLowerCase() === String(p.left).toLowerCase());
        const rty = right ? right.ty(p.right) : "";
        const lf = familyOf(l ? l.ty : "");
        const rf = familyOf(rty);
        const binary = lf === "bin" || rf === "bin";
        const cross =
          (lf === "num" && rf === "text") || (lf === "text" && rf === "num") || binary;
        if (!l || !cross) continue;
        const why = binary
          ? "有一边是二进制/大字段，取数时会被置成 NULL，而 NULL 连接键不可能匹配——这一轮 join 一行都配不上，做出来的表会是空表；换一对两边同族的列"
          : NEVER_INT_TY.includes(typeHead(lf === "text" ? l.ty : rty))
            ? "一边数值一边文本，而日期、时间戳、uuid、json 这类值永远写不成整数键——这一轮 join 一行都配不上，做出来的表会是空表；换一对两边同族的列"
            : "一边数值一边文本：join 按规范化的整数值比对，所以 bigint 1001 配得上文本「1001」；" +
              "但文本侧带前导零、小数点或空格就仍然配不上（「007」配不上 7），" +
              "这类不一致会静默少行甚至整轮空表——确认两边写法一致最稳妥";
        out.push(
          `数据集 ${ds.id}（${ds.name}）：${l.label}（${l.ty || "类型未知"}）与 ${p.right}（${rty || "类型未知"}）做连接键，${why}`
        );
      }
      if (!right) continue;
      const taken = acc.map((a) => a.name);
      for (const c of right.cols) {
        let n = c;
        let i = 2;
        while (taken.some((x) => x.toLowerCase() === n.toLowerCase())) n = `${c}_${i++}`;
        taken.push(n);
        acc.push({ name: n, label: `${j.source}.${c}`, ty: right.ty(c) });
      }
    }
  }
  return out;
}

function invoke(cmd: string, args: any): Promise<any> {
  push(cmd, args);
  const a = args || {};
  switch (cmd) {
    case "get_tables": {
      const cfg = a.config || {};
      return Promise.resolve((fixture as any).tables[cfg.id] || []);
    }
    case "report_describe_columns": {
      const t = String(a.table || "");
      // ?colslow=600 让列清单慢到"起草时还没落地"，用来验前置闸真的在等
      const slow = Number(window.__PROBE_ARG("colslow") || 0);
      // ?descfailonce=users 只失败第一次： transient 故障下"重读"该转好，
      // 用来验红标签真的能点回蓝色（只用 descfail 的话重试永远红，量不出闭环）
      if (window.__PROBE_ARG("descfailonce") === t && !failedOnce.has(t)) {
        failedOnce.add(t);
        return fail(`读取列清单失败：注入（表 ${t}，第 1 次）`);
      }
      const body = () => {
        if (window.__PROBE_ARG("descfail") === t) {
          return fail(`读取列清单失败：注入（表 ${t}）`);
        }
        if (window.__PROBE_ARG("notypes") === t) {
          // 整张表都探不到类型（真实 sqlite 无类型表就是这样）：提示词只能退回裸列名
          return Promise.resolve(described(t).map((c) => ({ ...c, data_type: "" })));
        }
        return Promise.resolve(described(t));
      };
      return slow > 0
        ? new Promise((r) =>
            setTimeout(() => {
              // 落地时刻要能被探针读到，否则"正在读取"闪没是注入没生效还是记账错了，分不开
              push("describe_done", { table: t, at: Math.round(performance.now()) });
              r(body());
            }, slow)
          )
        : body();
    }
    case "ai_sql_generate": {
      const catalog: any[] = a.catalog || [];
      const question = String(a.question || "");
      // 与 Rust generate() 同序的前置门槛
      if (!question.trim()) return fail("先描述你想查什么");
      if (!catalog.length) return fail("先选至少一张表，模型没有列清单就只能编字段");
      if (window.__PROBE_FLAG("sqlgenfail")) {
        return fail(
          `重试 2 次后仍未通过本机校验：SQL 里的表 invoicez 不在本次目录里（可用：${catalog
            .map((t) => t.table)
            .join(", ")}）`
        );
      }
      const out = mirrorDraft(question, catalog);
      if (out.reject) return fail(out.reject);
      return Promise.resolve(out.draft);
    }
    case "load_reports":
      if (window.__PROBE_FLAG("loadfail")) {
        return fail(
          "校验和不匹配：文件可能已损坏（expected=9f3a1c0000000000, got=7665a2f5783cd58a）"
        );
      }
      return Promise.resolve(reports);
    case "save_report": {
      const item = a.item;
      window.__PROBE_SAVED.push(JSON.parse(JSON.stringify(item)));
      if (window.__PROBE_FLAG("savefail")) {
        // 后端 check_report_shape 的真实拒因
        return fail("组件 ghost 绑定的数据集 no-such-ds 不存在（现有：city-gmv, paid-orders）");
      }
      reports = reports.filter((r) => r.id !== item.id);
      reports.push(JSON.parse(JSON.stringify(item)));
      reports.sort((x, y) => y.updated_at - x.updated_at);
      return Promise.resolve(null);
    }
    case "delete_report": {
      window.__PROBE_DELETE.push(a.id);
      if (window.__PROBE_FLAG("delfail")) return fail("删除失败：注入");
      reports = reports.filter((r) => r.id !== a.id);
      return Promise.resolve(null);
    }
    case "report_view_render":
    case "report_view_validate": {
      // 镜像 DbSource::resolve：源引用的连接不在前端传来的 configs 里就是这句拒因。
      // 不补这条，探针里把死连接改成任意字符串都会"取数成功"，改绑链路是个假绿灯。
      const alive = new Set((a.configs || []).map((c: any) => c.id));
      for (const ds of a.datasets || []) {
        for (const s of ds.sources || []) {
          if (!alive.has(s.connection_id)) {
            return fail(
              `源 ${s.alias} 引用的连接 ${s.connection_id} 不在本会话已解锁的连接里，请先在连接面板建立连接`
            );
          }
        }
      }
      return Promise.resolve(
        cmd === "report_view_render" ? (fixture as any).render : (fixture as any).validate
      );
    }
    case "ai_report_draft": {
      const cat: any[] = a.catalog || [];
      // 后端 draft() 的第一道门槛就是空问题；探针不镜像的话，
      // "前端拦住了"和"后端拒了"在探针里长得一模一样
      if (!String(a.question || "").trim()) return fail("请先描述你想要什么报表");
      const hole = cat.find((t) => !(t.columns || []).length);
      // 镜像 normalize_sources + source_columns：目录项列清单为空时，缓存里
      // 这张表就是零列，模型任何一次引用都过不了校验，三轮修光也没用。
      // 少了这条，探针里前端塞 columns: [] 也会"起草成功"，是个假绿灯。
      if (hole) {
        return fail(
          `重试 3 次后仍未通过本机校验：数据集 ds1：源 ${hole.table} 未声明列清单，也无法从 schema 缓存取到`
        );
      }
      // DraftResult.datasets 是 DatasetSpec[]（sources/aggregates/joins…）。
      // 这里曾误取 render.datasets —— 那是取数后的结果形状，没有 sources，
      // 起草一成功就在草稿卡里 map 崩掉整棵树，"起草成功"这条腿其实从没跑通过。
      const specs = (fixture as any).reports[0].datasets;
      const view = (fixture as any).reports[0].view;
      // 镜像 ai.rs 的两条 warning 规则：连接键跨族 + 没有组件在用的数据集。
      // 以前写死一句"跨 2 种方言"，把 users 丢掉之后它就不成立了。
      const used = new Set((view.widgets || []).map((x: any) => x.dataset));
      return Promise.resolve({
        datasets: specs,
        view,
        steps: ["scan orders (shop)", "filter status = 'paid'", "group by city", "join users (crm)"],
        columns: Object.fromEntries(specs.map((d: any) => [d.id, ["city", "gmv", "cnt"]])),
        warnings: [
          ...joinKeyWarnings(specs, cat),
          ...specs
            .filter((d: any) => !used.has(d.id))
            .map((d: any) => `数据集 ${d.id}（${d.name}）没有任何组件在用，执行时会白取一次数`),
        ],
        repairs: 0,
      });
    }
    // ================== SQL 工作台（App）启动所需 ==================
    case "load_connections":
      return Promise.resolve((fixture as any).connections);
    case "get_ai_config":
      return Promise.resolve((fixture as any).ai_config);
    case "load_query_history":
      return Promise.resolve((fixture as any).history || []);
    case "load_saved_queries":
      return Promise.resolve((fixture as any).saved_queries || []);
    case "test_connection":
      if (window.__PROBE_FLAG("connfail")) return fail("认证失败：注入");
      return Promise.resolve(null);
    case "get_table_structure": {
      const cols: string[] = (fixture as any).columns[a.tableName] || [];
      return Promise.resolve(
        cols.map((name, i) => ({
          name,
          data_type: name === "id" ? "int" : "varchar",
          nullable: name !== "id",
          is_primary: name === "id",
          default_value: null,
          ordinal: i,
        }))
      );
    }
    case "format_sql":
      return Promise.resolve(
        String(a.sql || "")
          .replace(/\s+/g, " ")
          .trim()
      );
    case "execute_query":
      return fail("探针不执行真实 SQL：请走 report_* 链路");
    // ================== 四条 AI 解说链路 ==================
    // 后端这四个命令都回纯文本；探针只验"参数键名与结构对不对"，
    // 因为 Tauri v2 会把 Rust 形参 snake_case 映射成 camelCase 载荷键。
    case "ai_optimize_sql":
    case "ai_explain_sql":
    case "ai_diagnose_error":
    case "ai_explain_results": {
      if (window.__PROBE_FLAG("aitextfail")) {
        return fail('AI 服务返回 401 Unauthorized：{"error":{"message":"invalid api key"}}');
      }
      const note =
        cmd === "ai_optimize_sql"
          ? `（收到 tableSchema ${Array.isArray(a.tableSchema) ? a.tableSchema.length : "缺失"} 项）`
          : cmd === "ai_explain_results"
            ? `（收到 ${Array.isArray(a.results) ? a.results.length : "缺失"} 行样例）`
            : "";
      return Promise.resolve(`${cmd} 的回答${note}\n1. 摘要\n2. 建议`);
    }
    default:
      return Promise.resolve(null);
  }
}

window.__TAURI_INTERNALS__ = { invoke };

export const CONTRACT = fixture;
