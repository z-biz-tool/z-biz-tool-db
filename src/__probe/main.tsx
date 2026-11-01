// 探针入口：直接挂 ReportWorkbench，绕开 SQL 工作台的无关 IPC。
import React from "react";
import ReactDOM from "react-dom/client";
import "./stub";
import { CONTRACT } from "./stub";
import ReportWorkbench from "../report/ReportWorkbench";
import { installDriver } from "./driver";
import { installReaders, installRoShim } from "./ro_shim";
import "../index.css";

// 内嵌浏览器视口恒为 0x0，recharts 量不到盒子就不画柱；
// 这里替一次 ResizeObserver 供给尺寸，只为验证组件本身正确。
installRoShim(900, 320);
installReaders();
installDriver();

(window as any).__CONTRACT = CONTRACT;

const configs = [
  {
    id: "shop",
    name: "商城库",
    db_type: "mysql",
    host: "127.0.0.1",
    port: 3306,
    username: "root",
    password: "",
    database: "shop",
  },
  {
    id: "crm",
    name: "客户库",
    db_type: "sqlite",
    host: "",
    port: 0,
    username: "",
    password: "",
    database: "crm.db",
  },
];

// 探针里的 AI 就绪状态：与真界面一致，由后端判（`get_ai_config_status`）。
// 探针用 stub 拦 invoke，所以这里直接给「已配置」。
const aiReady = true;

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <div style={{ height: "100vh", overflow: "auto" }}>
      <ReportWorkbench
        configs={configs}
        aiReady={aiReady}
        onOpenAiSettings={() => {
          (window as any).__PROBE_OPEN_AI = ((window as any).__PROBE_OPEN_AI || 0) + 1;
        }}
      />
    </div>
  </React.StrictMode>
);
