/**
 * 错误码契约（后端 db/ipc.rs ↔ 前端 ipc/errors.ts）。
 *
 * 跑法：node --experimental-strip-types --test tests/errorCodeContract.test.ts
 *
 * 为什么要这道门：后端错误串形如 `READONLY_BLOCKED: 只读模式拒绝 ...`。
 * 前端优先按**码**分类（比中文正则可靠），但两边各写一份清单 ——
 * 一旦只改一边，界面就会把"审批被拒"显示成"未知错误"。
 *
 * 这道门同时守住两件事：
 * 1) 前端 CODE_TABLE 里的码，后端 db/ipc.rs 里必须真的定义过；
 * 2) 后端定义的码，前端必须认（不许有后端发得出、前端接不住的码）。
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { classifyError } from "../src/ipc/errors.ts";

const here = fileURLToPath(new URL(".", import.meta.url));
const ipcRs = readFileSync(`${here}../src-tauri/src/db/ipc.rs`, "utf8");
const errorsTs = readFileSync(`${here}../src/ipc/errors.ts`, "utf8");

/** 后端定义的全部错误码 */
function backendCodes(): string[] {
  const out: string[] = [];
  const re = /pub const CODE_[A-Z0-9_]+: &str = "([A-Z0-9_]+)";/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(ipcRs)) !== null) out.push(m[1]);
  return out;
}

/** 前端 CODE_TABLE 里的全部码 */
function frontendCodes(): string[] {
  const out: string[] = [];
  const re = /^\s*([A-Z][A-Z0-9_]*):\s*\{\s*category:/gm;
  let m: RegExpExecArray | null;
  while ((m = re.exec(errorsTs)) !== null) out.push(m[1]);
  return out;
}

/**
 * 前端**自己**会产出的分类：后端永远不会发这些码，
 * 但前端要能通过中文正则归到这些类（例如 invoke 失败 ⇒ IPC_ERROR）。
 * 允许前端多，不允许前端少。
 */
const FRONTEND_ONLY_CODES = ["AUTHZ_DENIED", "SECRET_STRIPPED", "IPC_ERROR"];

test("后端确实定义了错误码清单", () => {
  const codes = backendCodes();
  assert.ok(codes.length >= 5, `后端错误码太少: ${codes.join(",")}`);
  for (const c of ["READONLY_BLOCKED", "APPROVAL_DENIED", "STALE_GENERATION", "INVALID_PORT"]) {
    assert.ok(codes.includes(c), `后端缺少错误码 ${c}`);
  }
});

test("后端能发出的码，前端必须全认（多一个都不行）", () => {
  const front = new Set(frontendCodes());
  const missingInFront = backendCodes().filter((c) => !front.has(c));
  assert.deepEqual(missingInFront, [], `前端没认这些后端码: ${missingInFront.join(",")}`);
});

test("前端多出来的码必须是有据可依的客户端分类", () => {
  const back = new Set(backendCodes());
  const extra = frontendCodes().filter((c) => !back.has(c));
  assert.deepEqual(
    extra.sort(),
    [...FRONTEND_ONLY_CODES].sort(),
    `前端多认的码变了，要么后端补上定义，要么更新 FRONTEND_ONLY_CODES 并说明理由: ${extra.join(",")}`
  );
});

test("带码的错误串按码分类（不再靠中文正则猜）", () => {
  const e = classifyError("READONLY_BLOCKED: 只读模式拒绝 Dml (safety=Write)");
  assert.equal(e.category, "readonly_blocked");
  assert.equal(e.code, "READONLY_BLOCKED");
  assert.equal(e.retryable, false);
  // 正文保留给人看
  assert.match(e.safe_message, /只读模式拒绝/);
});

test("STALE_GENERATION 可重试（结果过期，重试有意义）", () => {
  const e = classifyError("STALE_GENERATION: generation 不匹配：期望 2，实际 1");
  assert.equal(e.category, "stale_generation");
  assert.equal(e.retryable, true);
});

test("没带码的老错误串仍走中文正则（后端升级前的兼容）", () => {
  const e = classifyError("只读模式拒绝 Dml：kind=Dml, safety=Write");
  assert.equal(e.category, "readonly_blocked");
  assert.equal(e.code, "READONLY_BLOCKED");
});

test("未知错误不被硬套一个码", () => {
  const e = classifyError("something went sideways");
  assert.equal(e.category, "unknown");
  assert.equal(e.code, "UNKNOWN");
});
