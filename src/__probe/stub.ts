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

/** 后端 reject 出来的可能是纯文本，也可能是 DraftReject 那样的结构体，
 *  两种形状都要能注入——前端对这两者的处理路径不一样。 */
function fail(msg: string | object) {
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

/** 镜像 view.rs 的两条本机规则：组件绑的数据集在不在、编码点名的列在不在该数据集输出里。
 *  输出列取 fixture.validate.schemas——那是本机对这份 fixture 真算出来的结果，镜像不另猜一套。
 *  要点是"一次列全"而不是撞到第一处就停：否则探针看不出清单有几条，
 *  错误卡到底按不按行铺开也就无从判起。 */
function viewProblems(view: any, specs: any[]): string[] {
  const known = new Set(specs.map((d: any) => d.id));
  const schemas: Record<string, string[]> = (fixture as any).validate.schemas;
  const out: string[] = [];
  for (const w of view.widgets || []) {
    if (!known.has(w.dataset)) {
      out.push(
        `组件 ${w.id} 绑定的数据集 ${w.dataset} 不存在（现有：${[...known].join(", ") || "-"}）`
      );
      continue;
    }
    const cols = schemas[w.dataset];
    if (!cols) continue;
    const need = (what: string, col: any) => {
      if (typeof col !== "string" || !col) return;
      if (!cols.some((c) => c.toLowerCase() === col.toLowerCase()))
        out.push(
          `组件 ${w.id}（数据集 ${w.dataset}）：${what} 列 ${col} 不在数据集输出里（可用列：${cols.join(", ") || "-"}）`
        );
    };
    const e = w.encode || {};
    need("x", e.x);
    need("y", e.y);
    need("系列", e.series);
    need("扇区", e.category);
    need("数值", e.value);
    for (const c of e.columns || []) need("展示", c);
  }
  return out;
}

/** 与 view.rs problem_list 同措辞：一处就照原样说，多处才编号 */
function problemList(problems: string[]): string {
  if (problems.length === 1) return problems[0];
  return `共 ${problems.length} 处问题，一次全改完再跑：${problems
    .map((p, i) => `\n${i + 1}. ${p}`)
    .join("")}`;
}

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
      // ?notables=crm 让某个连接的表清单读不到：跨库提示不能因此就说死"别的库里也没有"
      if (String(window.__PROBE_ARG("notables") || "").split(",").includes(String(cfg.id))) {
        return fail(`读取表清单失败：注入（连接 ${cfg.id}）`);
      }
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
      // 镜像 ai_sql_generate → HttpModel::new：模型名空则在发请求之前就拒。
      // 不补这条，前端"先配置 AI"那道闸在探针里删掉也是绿的。
      if (!String((a.config || {}).model || "").trim()) {
        return fail({ error: "请先在设置中填写 AI 模型名", sql: null });
      }
      // 与 Rust generate() 同序的前置门槛
      if (!question.trim()) return fail({ error: "先描述你想查什么", sql: null });
      if (!catalog.length) return fail({ error: "先选至少一张表，模型没有列清单就只能编字段", sql: null });
      // ?sqlbroken=1 注入"模型回了一段说明而不是 SQL"：本机连底稿都挖不出来，
      // sql 是 null，错误卡上就不该挂「让 AI 照这条错误改」
      if (window.__PROBE_FLAG("sqlbroken")) {
        return fail({
          error: "重试 3 次后仍未通过本机校验：模型没给出 SQL，只回了一段说明",
          sql: null,
        });
      }
      // ?sqlx=users 注入"模型引用了一张本腿目录里没有的表"（可逗号列多张）：
      // 表名撞在别的连接里时，错误卡要把那个连接点名出来并给出 AI 报表入口；
      // 哪个连接都没有时只能提示"这是模型编的"，两半靠注入不同的名字分开量。
      if (window.__PROBE_ARG("sqlx")) {
        const names = String(window.__PROBE_ARG("sqlx"))
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean);
        return fail({
          error:
            `重试 3 次后仍未通过本机校验：SQL 里的表 ${names.join("、")} 不在本次目录里（可用：` +
            `${catalog.map((t) => t.table).sort().join(", ")}）`,
          sql: `SELECT * FROM ${names.join(", ")}`,
        });
      }
      // ?sqlgenfail=1 注入"模型编了一张不存在的表、且怎么改都改不对"：
      // 拒因和被拒的那条 SQL 两半都要在回执里（与后端 SqlReject 同形），
      // 探针靠这条腿量错误卡上有没有回喂入口、以及点下去之后 wire 上少了哪一半。
      if (window.__PROBE_FLAG("sqlgenfail")) {
        return fail({
          error: `重试 3 次后仍未通过本机校验：SQL 里的表 invoicez 不在本次目录里（可用：${catalog
            .map((t) => t.table)
            .join(", ")}）`,
          sql: "SELECT city, total FROM invoicez",
        });
      }
      // ?sqlboth=1 注入"表认不得 + 列认不得一次列全"那份清单（镜像 check_sql 走 problem_list）：
      // 前端错误卡要真按行铺开，一键改稿要把整份清单原样带回去——只带回第一条就过不了这一腿。
      if (window.__PROBE_FLAG("sqlboth")) {
        const ticket = String(a.feedback || "");
        if (!ticket.includes("invoicez") || !ticket.includes("orders.net_zz")) {
          return fail({
            error: problemList([
              `SQL 里的表 invoicez 不在本次目录里（可用：${catalog
                .map((t) => t.table)
                .sort()
                .join(", ")}）`,
              `SQL 用到了目录里不存在的列：orders.net_zz。这些表的真实列是：${catalog
                .map((t) => `${t.table} = [${(t.columns || []).join(", ")}]`)
                .join("；")}`,
            ]),
            sql: "SELECT orders.net_zz FROM invoicez",
          });
        }
      }
      // ?sqlreject=1 注入"模型编了一个不存在的列、本机把这条 SQL 挡下"：
      // 判"改对了"收紧到两半都对上——拒因要点名 net_zz，且带回来的底稿里真有这一列。
      // 只把 feedback 塞成任意非空字符串、或者拿上一轮成功那条当底稿，都还是过不了。
      if (window.__PROBE_FLAG("sqlreject")) {
        const t0 = catalog[0];
        const ticket = String(a.feedback || "");
        const base = String((a.prior || {}).sql || "");
        if (!ticket.includes("net_zz") || !base.includes("net_zz")) {
          return fail({
            error:
              `重试 3 次后仍未通过本机校验：SQL 用到了目录里不存在的列：${t0.table}.net_zz。` +
              `这些表的真实列是：${catalog
                .map((t) => `${t.table} = [${(t.columns || []).join(", ")}]`)
                .join("；")}`,
            sql: `SELECT ${t0.table}.net_zz FROM ${t0.table}`,
          });
        }
      }
      const out = mirrorDraft(question, catalog);
      if (out.reject) return fail({ error: out.reject, sql: null });
      return Promise.resolve(out.draft);
    }
    case "ai_report_pick_tables": {
      const cands: any[] = a.candidates || [];
      // 镜像后端 pick()：模型名空、空需求、本机一张表都没读到，都在发请求之前就拒
      if (!String((a.config || {}).model || "").trim()) {
        return fail({ error: "请先在设置中填写 AI 模型名", answer: null });
      }
      if (!String(a.question || "").trim()) {
        return fail({ error: "先描述你想要什么报表，才知道要挑哪些表", answer: null });
      }
      if (!cands.length) {
        return fail({
          error: "本机一张表都没读到：先连上数据库，或在报表目录里手工勾选",
          answer: null,
        });
      }
      // ?pick=crm.users,shop.invoicez 指定"模型这一轮回的是哪几张表"（连接.表，与候选同形）；
      // ?pick=none 是"一张都没挑"；?pickbroken=1 是"回的连 JSON 都不是"（没有答案可回喂）。
      if (window.__PROBE_FLAG("pickbroken")) {
        return fail({
          error: "本机核对没通过：模型回的既不是 JSON 也没法解析（注入）",
          answer: null,
        });
      }
      const tables = String(window.__PROBE_ARG("pick") || "")
        .split(",")
        .map((x) => x.trim())
        .filter((x) => x && x !== "none");
      const key = (c: any) => `${c.connection_id}.${c.table}`;
      const known = tables.filter((x) => cands.some((c) => key(c) === x));
      const unknown = tables.filter((x) => !known.includes(x));
      const answer = JSON.stringify({
        tables: tables.map((x) => {
          const [connection_id, table] = x.split(".");
          return { connection_id, table };
        }),
        reason: "先看成交额",
      });
      // 第二轮（前端把拒因和那份答案一起回喂了）当成模型照着改对了。
      // 判"改对了"要两半都在：只回错误原文、或只回那份答案，模型都无从下手。
      const fb = String(a.feedback || "").trim();
      const pa = String(a.priorAnswer || "").trim();
      if (unknown.length && !(fb && pa)) {
        const conns = [...new Set(cands.map((c) => c.connection_id))].sort();
        const problems = unknown.map((x) => {
          const [c, t] = x.split(".");
          const avail = [...new Set(cands.filter((y) => y.connection_id === c).map((y) => y.table))].sort();
          return avail.length
            ? `连接 ${c} 里没有表 ${t}（该连接的可用表：${avail.join(", ")}）`
            : `清单里没有连接 ${c}（可用连接：${conns.join(", ")}）`;
        });
        // 不说"重试 N 次"：这条腿只回了一次答案，真后端那几轮自我修正在一次 IPC 里，
        // 探针里学不出那个次数，写死只会让后面读探针的人以为量过它
        return fail({
          error: `本机核对没通过：${problemList(problems)}`,
          answer,
        });
      }
      // 改完还是没有任何一张表落在本机清单里：那还是没挑出可用的表，不能当成功放过去
      if (unknown.length && !known.length) {
        return fail({
          error: `本机核对没通过：清单里没有连接 ${[...new Set(unknown.map((x) => x.split(".")[0]))].join("、")}（可用连接：${[...new Set(cands.map((c) => c.connection_id))].sort().join(", ")}）`,
          answer,
        });
      }
      if (!unknown.length && !tables.length) {
        return fail({
          error: "本机核对没通过：模型一张表都没挑出来：换个说法，或者手工勾选",
          answer,
        });
      }
      const picked = (unknown.length ? known : tables.map((x) => x)).map((x) =>
        cands.find((c) => key(c) === x)
      );
      return Promise.resolve({
        picked,
        reason: "先看成交额",
        repairs: unknown.length ? 1 : 0,
        truncated: 0,
        warnings: [],
      });
    }
    // 镜像 ai_report_explain：先核材料，再要模型名；行数据不该出现在材料里
    case "ai_report_explain": {
      const b = a.brief || {};
      const ds = b.datasets || [];
      const failed = b.failed || [];
      if (!ds.length && !failed.length) {
        return fail("这张报表还没有可解释的材料：先点一次「取数并渲染」");
      }
      if (!String((a.config || {}).model || "").trim()) {
        return fail("请先在设置中填写 AI 模型名");
      }
      const sqls = ds.flatMap((d: any) => (d.sqls || []).map((q: any) => `${q.connection}:${q.sql}`));
      if (!sqls.length) return fail("材料里没有一条下推 SQL：这样讲出来的口径是编的");
      // ?explainfail=1 当成模型侧失败：错误要能在看板上看见，不能只闪一句 toast
      if (window.__PROBE_FLAG("explainfail")) return fail("AI 服务 500：模型侧炸了（注入）");
      const trunc = ds.filter((d: any) => (d.truncated || []).length).map((d: any) => d.id);
      return Promise.resolve(
        [
          `这张图由 ${ds.length} 个数据集算出来，一共下推了 ${sqls.length} 条 SQL：${sqls.join(" | ")}`,
          `问的是：${b.question || "（没带需求）"}`,
          `算子链 ${((b.steps || []).length)} 步`,
          trunc.length ? `其中 ${trunc.join("、")} 撞上取数上限，数只是取回的那部分` : "没有数据集撞取数上限",
        ].join("\n")
      );
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
      // 只给 report_view_validate 镜像本机的一条规则：组件绑的数据集在不在、
      // 编码里点名的列在不在该数据集输出里（后端 validate_view / need_col 的原话）。
      // 一次列全而不是撞到第一处就停——不然探针里根本看不出"这份清单有几条"，
      // 前端的错误卡是不是真按行铺开也无从判起。
      if (cmd === "report_view_validate") {
        const problems = viewProblems(a.view || {}, a.datasets || []);
        if (problems.length) return fail(problemList(problems));
      }
      // 镜像 report_view_render 的按数据集降级：?dsfail=city-gmv 让那张集取数失败，
      // 挂在它上面的组件被摘掉、其余照常出；全部摘光时后端是整单报错而不是回空板面。
      const raw = String((window as any).__PROBE_RUNFAIL ?? window.__PROBE_ARG("dsfail") ?? "");
      if (cmd === "report_view_render" && raw.trim()) {
        const dead = raw.split(",").map((s) => s.trim()).filter(Boolean);
        const specs: any[] = a.datasets || [];
        const known = new Set(specs.map((d: any) => d.id));
        const bogus = dead.filter((d) => !known.has(d));
        if (bogus.length) return fail(`注入的数据集 id 不在规格里：${bogus.join(",")}（现有：${[...known].join(",")}）`);
        const r = JSON.parse(JSON.stringify((fixture as any).render));
        const widgetsOf = (id: string) =>
          ((a.view || {}).widgets || []).filter((w: any) => w.dataset === id).map((w: any) => w.id);
        const lost = new Set(dead.flatMap(widgetsOf));
        const total = ((a.view || {}).widgets || []).length;
        if (total > 0 && lost.size >= total) {
          return fail(
            `${dead.length} 个数据集全部没取到数：${dead
              .map((id) => `「${specs.find((s: any) => s.id === id).name}」(${id})：注入的取数失败`)
              .join("；")}`
          );
        }
        r.failed = dead.map((id) => ({
          id,
          name: specs.find((s: any) => s.id === id).name,
          error: "注入的取数失败：SQLite 文件不存在",
          widgets: widgetsOf(id),
        }));
        r.charts = r.charts.filter((c: any) => !lost.has(c.widget));
        r.layout = r.layout.filter((l: any) => !lost.has(l.widget));
        r.datasets = r.datasets.filter((d: any) => !dead.includes(d.id));
        r.steps = r.steps.filter((s: string) => !dead.some((id) => s.includes(`dataset=${id}`)));
        return Promise.resolve(r);
      }
      // 取数每次回的都是新的一份（真后端每次重新组包）。复用同一个对象会让
      // "payload 变了没"这种判断在探针里失灵——旧解释该不该作废就量不出来
      if (cmd !== "report_view_render") return Promise.resolve((fixture as any).validate);
      const out = JSON.parse(JSON.stringify((fixture as any).render));
      // ?trunc=city-gmv 当成"这张集撞上 max_rows"：payload.partial 与每集的 truncated/rows
      // 造不出来，就量不到"就地提高上限"那条腿
      const trunc = String(window.__PROBE_ARG("trunc") || "")
        .split(",")
        .map((x) => x.trim())
        .filter(Boolean);
      if (trunc.length) {
        const want = (a.datasets || []).find((d: any) => trunc.includes(d.id));
        const cap = Number(want?.max_rows ?? 50000);
        for (const d of out.datasets) {
          if (!trunc.includes(d.id)) continue;
          d.partial = true;
          d.truncated = [want?.sources?.[0]?.alias || "o"];
          d.rows = cap;
        }
        out.partial = true;
      }
      return Promise.resolve(out);
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
      // ?draftbroken=1 注入"模型压根没吐出可解析的 JSON"：这种拒没有底稿可带
      // （DraftReject.draft 是 null），错误卡上就不该再挂「照这条错误改」。
      if (window.__PROBE_FLAG("draftbroken")) {
        return fail("重试 3 次后仍未通过本地校验：回复里找不到 JSON 对象（模型回复被截断了）");
      }
      // ?draftx=crm.users 注入"这一稿用了一张没勾进报表目录的表"（镜像 DraftReject：拒因 + 被拒那一稿）。
      // 判"这一轮该不该再挡"只看目录里现在有没有这张表：挂在哪个连接下都算，
      // 因为「加进目录并重问」这一键补的是目录——下一轮 wire 上的目录里真有了它，才算闭环。
      if (window.__PROBE_ARG("draftx")) {
        const [conn, table] = String(window.__PROBE_ARG("draftx")).split(".");
        const named = cat.some(
          (t: any) => String(t.table).toLowerCase() === String(table).toLowerCase()
        );
        if (!named) {
          const d = JSON.parse(JSON.stringify((fixture as any).reports[0]));
          // 只留"第一张源 + 注入那张"：底稿里再留着别的没勾进目录的表，
          // 探针就会量出一条拒因根本没点名的补表入口（假阳性，且是真做不出的事）
          const ds0 = d.datasets[0];
          const second = ds0.sources[1] || { ...ds0.sources[0], alias: "x1" };
          ds0.sources = [ds0.sources[0], { ...second, connection_id: conn, table }];
          ds0.joins = (ds0.joins || []).filter((j: any) =>
            ds0.sources.some((s: any) => s.alias === j.source)
          );
          const avail =
            ((fixture as any).tables[conn] || []).map((t: any) => t.name).join(", ") || "无";
          return fail({
            error:
              `重试 3 次后仍未通过本机校验：数据集 ${ds0.id}：` +
              `连接 ${conn} 里没有表 ${table}（可用表：${avail}）`,
            draft: { datasets: d.datasets, view: d.view },
          });
        }
      }
      // ?draftreject=1 注入"模型编了字段、本机把这一稿挡下来"：
      // 拒因里点名的那一列只在被拒的那一稿里存在，所以回执必须是 {error, draft} 两半，
      // 与后端 DraftReject 同形。探针里要靠这条腿量两件事：
      //   1. 错误卡上有没有「让 AI 照这条错误改」这个入口
      //   2. 点它之后 wire 上有没有同时出现错误原文和被拒的那一稿
      // 判"改对了"的条件也收紧到这两半都对上：只把 feedback 塞成任意非空字符串、
      // 或者拿编辑器里那份干净设计当底稿，都还是过不了。
      // 也认运行时开关：链条出图之后要再演一次"起草被拒"，只能中途改条件（URL 开关改不了）
      if (window.__PROBE_FLAG("draftreject") || (window as any).__PROBE_DRAFTREJECT === true) {
        const d = JSON.parse(JSON.stringify((fixture as any).reports[0]));
        const bad = d.view.widgets.find((x: any) => x.type === "BAR");
        bad.encode.y = "profit_zz";
        const ticket = String(a.feedback || "");
        const base = ((a.prior || {}).draft || {}) as any;
        const baseHasBad = (base.view?.widgets || []).some((x: any) => x.encode?.y === "profit_zz");
        if (!ticket.includes("profit_zz") || !baseHasBad) {
          return fail({
            error:
              `重试 3 次后仍未通过本机校验：组件 ${bad.id}（数据集 ${bad.dataset}）：` +
              `y 列 profit_zz 不在数据集输出里（可用列：${
                ((fixture as any).validate.schemas[bad.dataset] || []).join(", ") || "-"
              }）`,
            draft: { datasets: d.datasets, view: d.view },
          });
        }
      }
      // DraftResult.datasets 是 DatasetSpec[]（sources/aggregates/joins…）。
      // 这里曾误取 render.datasets —— 那是取数后的结果形状，没有 sources，
      // 起草一成功就在草稿卡里 map 崩掉整棵树，"起草成功"这条腿其实从没跑通过。
      const specs = (fixture as any).reports[0].datasets;
      const view = (fixture as any).reports[0].view;
      // ?draftmut=drop_widget|drop_dataset|add_widget|tweak 让"模型改坏了"可注入。
      // URL 参数只能整页重载才有变，追问是同一会话里的第二轮，所以再认一个运行期钩子。
      const mut = String(
        (window as any).__PROBE_DRAFTMUT ?? window.__PROBE_ARG("draftmut") ?? ""
      );
      const nextSpecs = JSON.parse(JSON.stringify(specs));
      const nextView = JSON.parse(JSON.stringify(view));
      if (mut === "drop_widget" && nextView.widgets.length > 1) nextView.widgets.pop();
      if (mut === "add_widget")
        nextView.widgets.push({
          id: "w_probe",
          type: "TABLE",
          title: "注入的新组件",
          dataset: nextSpecs[0].id,
        });
      if (mut === "drop_dataset" && nextSpecs.length > 1) {
        const gone = nextSpecs.pop();
        nextView.widgets = nextView.widgets.filter((x: any) => x.dataset !== gone.id);
      }
      if (mut === "tweak") nextSpecs[0].limit = 7;
      // 镜像 ai.rs 的两条 warning 规则：连接键跨族 + 没有组件在用的数据集。
      // 以前写死一句"跨 2 种方言"，把 users 丢掉之后它就不成立了。
      const used = new Set(nextView.widgets.map((x: any) => x.dataset));
      return Promise.resolve({
        datasets: nextSpecs,
        view: nextView,
        steps: ["scan orders (shop)", "filter status = 'paid'", "group by city", "join users (crm)"],
        columns: Object.fromEntries(nextSpecs.map((d: any) => [d.id, ["city", "gmv", "cnt"]])),
        warnings: [
          ...joinKeyWarnings(nextSpecs, cat),
          ...nextSpecs
            .filter((d: any) => !used.has(d.id))
            .map((d: any) => `数据集 ${d.id}（${d.name}）没有任何组件在用，执行时会白取一次数`),
        ],
        repairs: 0,
      });
    }
    // ================== SQL 工作台（App）启动所需 ==================
    case "load_connections":
      // ?onlyconn=shop 让工作区里只剩一条连接：这时"表在别的库里吗"根本无从问起，
      // 那块跨库提示必须整块不出现（没有别的连接也硬问一句，就是白刷一次库）
      return Promise.resolve(
        window.__PROBE_ARG("onlyconn")
          ? (fixture as any).connections.filter(
              (c: any) => String(c.id) === window.__PROBE_ARG("onlyconn")
            )
          : (fixture as any).connections
      );
    case "get_ai_config":
      // ?noaicfg=1 当成"还没配过 AI"：没有这条注入，前端那道
      // "先配置 AI 服务地址、密钥与模型"的闸在探针里删掉也量不出来
      return Promise.resolve(window.__PROBE_FLAG("noaicfg") ? null : (fixture as any).ai_config);
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
    case "execute_query": {
      // 真实后端会跑这条 SQL 并回 tagged 单元格；探针按同一种形状回，
      // 这样"成功/失败各留什么现场"才验得动。?execfail=1 换成一条带驱动原文的报错。
      if (window.__PROBE_FLAG("execfail")) {
        return fail(
          "error returned from database: 1146 (42S02): Table 'shop.orders_x' doesn't exist"
        );
      }
      // ?rows=N 造 N 行结果；带 max_rows 时按后端同一条口径截断
      // （先取完再截，所以 total_rows 是整批行数）
      const n = Number(window.__PROBE_ARG("rows") || 1);
      const all = Array.from({ length: Math.max(1, n) }, (_, i) => [
        { __kind: "integer", value: String(i + 1) },
        { __kind: "text", value: i === 0 ? "杭州" : `城市${i + 1}` },
        { __kind: "decimal", value: `${99 + i}.50` },
      ]);
      const cap = Number(a.maxRows ?? a.max_rows ?? 0);
      const cut = cap > 0 && all.length > cap;
      // 与 sqlite 流式那条同口径：取到上限就停，总行数没数过 → 0 = 未知（不是 0 行）。
      // ?totalknown=1 当成 mysql/postgres 那半边（整批取回后截断，总数是准的）
      const reportedTotal = cut && !window.__PROBE_FLAG("totalknown") ? 0 : all.length;
      return Promise.resolve({
        id: "probe-run-1",
        columns: ["id", "city", "amount"],
        truncated: cut,
        total_rows: reportedTotal,
        column_meta: [
          { ordinal: 0, name: "id", native_type: "BIGINT", logical_type: "integer", nullable: false },
          { ordinal: 1, name: "city", native_type: "VARCHAR", logical_type: "text", nullable: true },
          { ordinal: 2, name: "amount", native_type: "DECIMAL", logical_type: "decimal", nullable: true },
        ],
        rows: cut ? all.slice(0, cap) : all,
        affected_rows: 0,
        execution_time_ms: 7,
        is_query: true,
        timings: { connect_ms: 2, queue_ms: 0, execute_ms: 4, fetch_ms: 1, total_ms: 7 },
      });
    }
    // ================== 四条 AI 解说链路 ==================
    // 后端这四个命令都回纯文本；探针只验"参数键名与结构对不对"，
    // 因为 Tauri v2 会把 Rust 形参 snake_case 映射成 camelCase 载荷键。
    case "ai_optimize_sql":
    case "ai_explain_sql":
    case "ai_diagnose_error":
    case "ai_explain_results": {
      // 镜像 lib.rs 里这四条命令共用的第一道门槛：缺地址或密钥就在发请求前拒。
      // 没有这条，前端"先配置 AI"那道闸删掉探针照样绿。
      const cfg = a.config || {};
      if (!cfg.base_url || !cfg.api_key) {
        return fail("请先在设置中配置 AI 参数");
      }
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
