// 探针入口：挂载整只 SQL 工作台（App）。
// 用来验 AI 生成 SQL 这条真实链路：选连接 → 连库 → 勾选表 → 生成 → 编辑器落字。
import React from "react";
import ReactDOM from "react-dom/client";
import "./stub";
import { CONTRACT } from "./stub";
import { installRoShim, installReaders } from "./ro_shim";
import { installDriver } from "./driver";
import App from "../App";
import "../index.css";

installRoShim(900, 320);
installReaders();
// callsOf / genCatalogSent 这些读数的口子两个探针入口都要有，
// 不然"前端把类型送到了没有"只能靠猜。
installDriver();
(window as any).__CONTRACT = CONTRACT;

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <div style={{ height: "100vh", overflow: "auto" }}>
      <App />
    </div>
  </React.StrictMode>
);
