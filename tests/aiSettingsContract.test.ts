/**
 * AI 配置草稿的**键名契约**：前端适配器送的键，必须与后端 serde 认的键一致。
 *
 * ## 为什么这道门必须有
 *
 * Tauri v2 只对**顶层命令参数**做 snake_case → camelCase 映射；
 * **嵌套结构体走 serde**，键名以结构体上的 `#[serde(rename_all = ...)]` 为准。
 *
 * 键名对不上的代价**不是报错**，而是 serde 静默忽略 ⇒
 * `base_url` 变空串、`api_key` 变空串 ⇒
 * 用户明明填了地址和密钥，点「保存」后配置里却是空的，
 * 而且**没有任何报错能暴露它** —— 面板说「已保存」，事实是没存进去。
 *
 * 这不是假想：db 之外的 kb / file 两仓此前就踩在这上面
 * （适配器送 snake_case，后端结构体标的是 camelCase），
 * 它们各自的测试全绿，功能却是坏的。
 *
 * ## 这道门怎么量
 *
 * 两边都从**源码**抽，不手抄名单：
 * - 后端：解析 `src-tauri/src/llm_shared.rs` 里 `AiConfigDraft` 的
 *   字段与 `rename_all`；
 * - 前端：解析 `src/components/AiSettingsPanelAdapter.tsx` 里
 *   `draftToConfig` 返回对象的键。
 *
 * 然后按后端的 rename_all 规则换算出「后端认的键」，逐字比对。
 *
 * 变异对照：把适配器里的 `baseUrl` 改回 `base_url` ⇒ 这条立刻红。
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const here = fileURLToPath(new URL(".", import.meta.url));
const repoRoot = path.resolve(here, "..");

const RUST_FILE = path.join(repoRoot, "src-tauri/src/llm_shared.rs");
const ADAPTER_FILE = path.join(repoRoot, "src/components/AiSettingsPanelAdapter.tsx");

/** 剥注释，避免文档注释里的字样被当成代码。 */
function stripComments(src: string): string {
  return src.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
}

function toCamel(s: string): string {
  return s.replace(/_([a-z0-9])/g, (_, c: string) => c.toUpperCase());
}

/** 后端 `AiConfigDraft`：字段名 + serde 的 rename_all 策略。 */
interface RustDraft {
  fields: string[];
  renameAll: "camelCase" | "snake_case" | null;
}

function parseRustDraft(): RustDraft {
  const src = stripComments(readFileSync(RUST_FILE, "utf8"));
  const structRe =
    /#\[serde\(rename_all\s*=\s*"(\w+)"\)\]\s*(?:#\[[^\]]*\]\s*)*pub struct AiConfigDraft\s*\{/g;
  const m = structRe.exec(src);
  assert.ok(m, "后端 llm_shared.rs 里找不到带 rename_all 的 AiConfigDraft 结构体");
  const renameAll = m[1] as RustDraft["renameAll"];

  // 从结构体开头往后找配对的 `}`
  const body = src.slice(m.index + m[0].length);
  let depth = 1;
  let end = -1;
  for (let i = 0; i < body.length; i++) {
    if (body[i] === "{") depth++;
    else if (body[i] === "}") {
      depth--;
      if (depth === 0) {
        end = i;
        break;
      }
    }
  }
  assert.ok(end > 0, "AiConfigDraft 结构体没闭合");
  const fields: string[] = [];
  for (const line of body.slice(0, end).split("\n")) {
    const fm = line.match(/^\s*pub\s+([A-Za-z_][A-Za-z0-9_]*)\s*:/);
    if (fm) fields.push(fm[1]);
  }
  return { fields, renameAll };
}

/** 前端 `draftToConfig` 返回对象的键。 */
function parseAdapterKeys(): string[] {
  const src = stripComments(readFileSync(ADAPTER_FILE, "utf8"));
  const fnRe = /function draftToConfig\s*\([^)]*\)\s*:\s*[A-Za-z_][A-Za-z0-9_]*\s*\{/;
  const m = fnRe.exec(src);
  assert.ok(m, "适配器里找不到 draftToConfig 函数");
  const body = src.slice(m.index + m[0].length);
  // 取第一个 `return { ... }` 字面量
  const ret = body.match(/return\s*\{([\s\S]*?)\n\s*\};/);
  assert.ok(ret, "draftToConfig 里找不到返回对象字面量");
  const keys: string[] = [];
  for (const line of ret[1].split("\n")) {
    const km = line.match(/^\s*([A-Za-z_][A-Za-z0-9_]*)\s*:/);
    if (km) keys.push(km[1]);
  }
  return keys;
}

const rust = parseRustDraft();
const adapterKeys = parseAdapterKeys();

/** 后端「认」的键：按 rename_all 换算。 */
function expectedKeys(): string[] {
  if (rust.renameAll === "camelCase") return rust.fields.map(toCamel);
  return rust.fields;
}

test("量具本身有效：两边都抽到了东西（不是空跑）", () => {
  assert.ok(rust.fields.length >= 5, `后端字段太少，解析器可能坏了: ${rust.fields.length}`);
  assert.ok(adapterKeys.length >= 5, `适配器键太少，解析器可能坏了: ${adapterKeys.length}`);
  assert.equal(rust.renameAll, "camelCase", "后端草稿结构体的 serde 策略变了，这条门禁要重测");
});

test("适配器送的草稿键，与后端 serde 认的键逐字一致", () => {
  // 后端认的键（按 rename_all 换算）
  const expected = new Set(expectedKeys());
  const wrong = adapterKeys.filter((k) => !expected.has(k));
  assert.deepEqual(
    wrong,
    [],
    `适配器送了后端不认的键: ${wrong.join(", ")}` +
      `（serde 会静默忽略 ⇒ 字段变默认值 ⇒ 用户填的地址/密钥根本没存进去）`
  );
});

test("适配器不多送键（多送的键会被 serde 忽略，等于假字段）", () => {
  const expected = new Set(expectedKeys());
  const extra = adapterKeys.filter((k) => !expected.has(k));
  assert.deepEqual(extra, [], `适配器多送了键: ${extra.join(", ")}`);
});

test("关键字段一个都不能漏（漏了 ⇒ 面板上的项存不进去）", () => {
  // 这三个是面板上用户真的会填的东西。少任何一个，用户都以为存了、其实没存。
  for (const key of ["baseUrl", "model", "apiKey"]) {
    assert.ok(
      adapterKeys.includes(key),
      `适配器没送 ${key} ⇒ 面板上这一项永远存不进去`
    );
  }
});

test("阴性对照：snake_case 的键与 camelCase 的键不是一回事", () => {
  // 键名规则被搞混时这条要能现形。kb / file 两仓此前就是送 snake_case，
  // 而后端结构体标的是 camelCase ⇒ 全部静默丢失。
  const camelKey = "baseUrl";
  const snakeKey = "base_url";
  const expected = new Set(expectedKeys());
  assert.ok(expected.has(camelKey), "后端认的键里应当有 baseUrl");
  assert.ok(!expected.has(snakeKey), "后端不该认 snake_case 的 base_url（否则规则变了）");
});
