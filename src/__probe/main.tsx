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

const aiConfig = { base_url: "https://api.example.com", api_key: "sk-probe", model: "probe-model" };

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <div style={{ height: "100vh", overflow: "auto" }}>
      <ReportWorkbench
        configs={configs}
        aiConfig={aiConfig}
        onOpenAiSettings={() => {
          (window as any).__PROBE_OPEN_AI = ((window as any).__PROBE_OPEN_AI || 0) + 1;
        }}
      />
    </div>
  </React.StrictMode>
);
