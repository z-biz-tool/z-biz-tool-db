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
      if (window.__PROBE_ARG("descfail") === t) {
        return fail(`读取列清单失败：注入（表 ${t}）`);
      }
      return Promise.resolve((fixture as any).columns[t] || []);
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
      return Promise.resolve((fixture as any).render);
    case "report_view_validate":
      return Promise.resolve((fixture as any).validate);
    case "ai_report_draft":
      return Promise.resolve({
        datasets: (fixture as any).render.datasets,
        // fixture 的 view 挂在报表条目上；这里曾误取 fixture.view（不存在），
        // 会让起草链路拿到 undefined 而假通过
        view: (fixture as any).reports[0].view,
        steps: [],
        columns: {},
        warnings: [],
        repairs: 0,
      });
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
    default:
      return Promise.resolve(null);
  }
}

window.__TAURI_INTERNALS__ = { invoke };

export const CONTRACT = fixture;
