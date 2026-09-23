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
  }
}

const q = new URLSearchParams(window.location.search);
window.__PROBE_FLAG = (k: string) => q.get(k) === "1";

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

function invoke(cmd: string, args: any): Promise<any> {
  push(cmd, args);
  const a = args || {};
  switch (cmd) {
    case "get_tables": {
      const cfg = a.config || {};
      return Promise.resolve((fixture as any).tables[cfg.id] || []);
    }
    case "report_describe_columns":
      return Promise.resolve((fixture as any).columns[a.table] || []);
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
        view: (fixture as any).view,
        steps: [],
        columns: {},
        warnings: [],
        repairs: 0,
      });
    default:
      return Promise.resolve(null);
  }
}

window.__TAURI_INTERNALS__ = { invoke };

export const CONTRACT = fixture;
