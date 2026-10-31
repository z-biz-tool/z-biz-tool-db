/**
 * db 仓深链语义层的单测。
 *
 * 跑法：node --experimental-strip-types --test tests/deepLink.test.ts
 *
 * 语义层是纯函数，所以这里不碰 Tauri、不碰 store，只喂 Handoff 看结果。
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { resolveDbDeepLink } from "../src/_shared/deepLink.ts";
import type { Handoff } from "z-biz-tool-shared/protocol";

function handoff(action: string, params: Record<string, string> = {}): Handoff {
  return { product: "db", action, params };
}

test("connect 带 conn：定位到该连接", () => {
  const out = resolveDbDeepLink(handoff("connect", { conn: "c-42" }));
  assert.equal(out.problem, null);
  assert.equal(out.connectionId, "c-42");
});

test("open 等价于 connect（action 缺省也按 open 处理）", () => {
  const out = resolveDbDeepLink(handoff("", { conn: "c-42" }));
  assert.equal(out.problem, null);
  assert.equal(out.connectionId, "c-42");
});

test("name 是可读兜底（发深链的人只有连接名）", () => {
  const out = resolveDbDeepLink(handoff("connect", { name: "生产库" }));
  assert.equal(out.problem, null);
  assert.equal(out.connectionName, "生产库");
  assert.equal(out.connectionId, null);
});

test("id 与 name 同时给时以 id 为准，name 一并保留", () => {
  const out = resolveDbDeepLink(handoff("connect", { conn: "c-42", name: "生产库" }));
  assert.equal(out.connectionId, "c-42");
  assert.equal(out.connectionName, "生产库");
});

test("既没有 conn 也没有 name：接不住并说明原因", () => {
  const out = resolveDbDeepLink(handoff("connect"));
  assert.equal(out.connectionId, null);
  assert.equal(out.connectionName, null);
  assert.match(out.problem ?? "", /conn|name/);
});

test("不认识的动作：接不住并说明是哪个动作", () => {
  const out = resolveDbDeepLink(handoff("drop", { conn: "c-42" }));
  assert.match(out.problem ?? "", /drop/);
  assert.equal(out.connectionId, null);
});

test("from 参数透传出来（用于提示与日志）", () => {
  const out = resolveDbDeepLink(handoff("connect", { conn: "c-42", from: "box" }));
  assert.equal(out.from, "box");
});

test("标识前后空格被裁掉（深链解析容易带上）", () => {
  const out = resolveDbDeepLink(handoff("connect", { conn: "  c-42  " }));
  assert.equal(out.connectionId, "c-42");
});

test("空字符串等同于没有标识", () => {
  const out = resolveDbDeepLink(handoff("connect", { conn: "   " }));
  assert.equal(out.connectionId, null);
  assert.ok(out.problem);
});
