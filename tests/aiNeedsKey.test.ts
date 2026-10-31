// db 仓前端「AI 配置够不够」判据的契约回归。
// 跑法：node --experimental-strip-types --test tests/aiNeedsKey.test.ts
//
// ## 为什么要有这条
//
// 后端 `report::ai::ensure_configured()`（src-tauri/src/report/ai.rs）已经
// 统一了判定：地址必填，**私网地址允许空密钥**（那类服务没有鉴权层）。
//
// 前端也得判同一件事（三个入口 + 工作台的 aiReady），否则出现**判据分裂**：
// 后端放行、前端却弹「先配置 AI 服务地址、密钥与模型」，把指向 mini 网关的
// 用户挡在门外，而**没有任何编译错误**能暴露它。
//
// 金标：Rust `cap_llm::is_private_endpoint()`。
// 共享层 `z-biz-tool-shared/ai/endpoint` 是它的逐条镜像。
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  isAiConfigured,
  aiNeedsApiKey,
} from "../src/report/aiNeedsKey.ts";

const camel = (over: Partial<{ baseUrl: string; apiKey: string; model: string }> = {}) => ({
  baseUrl: "http://192.168.31.249:8200/v1",
  apiKey: "",
  model: "zBiz-Text-35B",
  ...over,
});

const snake = (over: Partial<{ base_url: string; api_key: string; model: string }> = {}) => ({
  base_url: "http://192.168.31.249:8200/v1",
  api_key: "",
  model: "zBiz-Text-35B",
  ...over,
});

test("私网地址 + 空密钥：够（自建服务没有鉴权层）", () => {
  // 这是本次修的核心缺口：此前一律要 key ⇒ 本地网关用户连保存都保存不了。
  assert.equal(isAiConfigured(camel()), true);
});

test("私网地址 + 空密钥（蛇形形状）：一样够", () => {
  // ReportWorkbench 用后端形状的 aiConfig —— 两个形状必须同结论。
  assert.equal(isAiConfigured(snake()), true);
});

test("公网地址 + 空密钥：不够", () => {
  assert.equal(isAiConfigured(camel({ baseUrl: "https://api.openai.com/v1" })), false);
});

test("公网地址 + 有密钥：够", () => {
  assert.equal(
    isAiConfigured(camel({ baseUrl: "https://api.openai.com/v1", apiKey: "sk-x" })),
    true,
  );
});

test("地址为空：不够（哪怕密钥填了）", () => {
  assert.equal(isAiConfigured(camel({ baseUrl: "" })), false);
  assert.equal(isAiConfigured(camel({ baseUrl: "   " })), false);
});

test("模型为空：不够", () => {
  assert.equal(isAiConfigured(camel({ model: "" })), false);
});

test("要 Key 与够用必须同源，不能各说各话", () => {
  for (const c of [camel(), camel({ baseUrl: "https://api.openai.com/v1" })]) {
    assert.equal(aiNeedsApiKey(c), !isAiConfigured(c));
  }
});

test("阴性对照：同一判据对私网与公网必须给出相反结论", () => {
  // 判据被改成永真时这条立刻红。而恒真的判据会让本地网关用户被静默放行、
  // 公网用户被静默拦下 —— 两种故障都不会有人来报。
  const priv = isAiConfigured(camel());
  const pub = isAiConfigured(camel({ baseUrl: "https://api.openai.com/v1" }));
  assert.notEqual(priv, pub, "私网与公网必须给出相反结论");
  assert.equal(priv, true);
  assert.equal(pub, false);
});
