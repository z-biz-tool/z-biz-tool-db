/**
 * 摘要契约：前端 `digestSql` ↔ 后端 `security::digest_sql`。
 *
 * 跑法：node --experimental-strip-types --test tests/digestContract.test.ts
 *
 * 为什么这道门：**审批摘要是一对一的**（后端核对 `grant.sql_digest == digest_sql(sql)`），
 * 两端算得不一样 ⇒ 用户明明批过了，后端却报 "审批 sql_digest 与请求不匹配" ⇒
 * 合法写入被误拒，而且用户完全看不出是自己点错了还是程序坏了。
 *
 * 2026-10-22 实测踩过的坑：后端用 `sql.len()`（**字节数**），
 * 前端用 `sql.length`（**UTF-16 单元**）⇒ 含中文的 SQL 两端必然不一致。
 * 同一个函数内部也不自洽：head 取 `chars()`（码点），len 取字节。
 *
 * 这道门守三件事：
 * 1) 两端对同一批输入算出**同一个字面量**（黄金向量，含非 ASCII 与 emoji ——
 *    只有 ASCII 时三种长度口径结果一样，测不出问题）；
 * 2) 后端源码里的长度口径**不是**字节数（防止有人把 `chars().count()` 改回 `len()`）；
 * 3) CSV 导入的 canonical 格式**没被单方面改过**（后端改了格式 ⇒ 前端跟着改，
 *    而 canonical 是审批绑定的东西，格式一变审批绑定的东西就变了）。
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

import { digestSql, isApprovalUsable } from "../src/ipc/db.ts";

const here = fileURLToPath(new URL(".", import.meta.url));
const repoRoot = path.resolve(here, "..");

/**
 * 黄金向量。
 *
 * ⚠ 这里的字面量与 `src-tauri/src/security.rs` 的 `digest_golden_vectors`
 * 是**同一批**。任何一端改了摘要格式，另一端必红。
 * 长度口径 = **Unicode 码点数**（Python `len()` 的口径，可用 `python3 -c` 复核）。
 */
const VECTORS: Array<[string, string]> = [
  ["SELECT 1", "len=8|head=SELECT 1"],
  ["SELECT '中'", "len=10|head=SELECT '中'"],
  ["INSERT INTO t VALUES ('😀')", "len=26|head=INSERT INTO t VALUES ('😀')"],
  ["CSV_IMPORT table=users rows=3", "len=29|head=CSV_IMPORT table=users rows=3"],
];

test("黄金向量：前端摘要与后端字面量一致", () => {
  for (const [input, expected] of VECTORS) {
    assert.equal(digestSql(input), expected, `摘要对不上: ${JSON.stringify(input)}`);
  }
});

test("长度口径是码点数，不是字节数、也不是 UTF-16 单元", () => {
  // "中"：1 码点 / 3 字节 / 1 UTF-16 单元
  assert.ok(digestSql("中").startsWith("len=1|"), "不能按字节数算");
  // "😀"：1 码点 / 4 字节 / 2 UTF-16 单元 —— 这一条能把三种口径全分开
  assert.ok(digestSql("😀").startsWith("len=1|"), "emoji 算 1 个码点");
  assert.ok(!digestSql("😀").startsWith("len=2|"), "不能按 UTF-16 单元算");
  assert.ok(!digestSql("😀").startsWith("len=4|"), "不能按字节数算");
});

test("超过 64 个字符时 head 截断（按字符，不是字节）", () => {
  const long = "a".repeat(70);
  assert.equal(digestSql(long), `len=70|head=${"a".repeat(64)}`);
  // 非 ASCII 也要按字符截断
  const cjk = "中".repeat(70);
  assert.equal(digestSql(cjk), `len=70|head=${"中".repeat(64)}`);
});

test("审批可用性判定与摘要口径一致", () => {
  const sql = "SELECT '中'";
  const grant = {
    approval_id: "a1",
    sql_digest: digestSql(sql),
    environment: "dev",
    issued_at: 0,
    expires_at: 999,
    consumed: false,
  };
  assert.equal(isApprovalUsable(grant, sql, 0).ok, true);
  // 换一条 SQL ⇒ 摘要对不上 ⇒ 不可用
  assert.equal(isApprovalUsable(grant, "SELECT '中文'", 0).ok, false);
});

/* ────────────────────── 跨语言：后端源码口径 ────────────────────── */

/**
 * 剥掉 Rust 注释。
 *
 * 为什么必须剥：`digest_sql` 的**注释里**就写着 "这里原来用 `sql.len()`（字节数）"
 * ——那是解释历史，不是代码。不剥注释的话这道门会对着注释报警，
 * 等于逼着后人把这段有用的说明删掉。判据只该看**代码**。
 */
function stripRustComments(src: string): string {
  return src.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
}

const rustSecurity = stripRustComments(
  readFileSync(path.join(repoRoot, "src-tauri/src/security.rs"), "utf8")
);
const rustCsv = stripRustComments(
  readFileSync(path.join(repoRoot, "src-tauri/src/db/csv_import.rs"), "utf8")
);

test("后端 digest_sql 的长度口径不是字节数", () => {
  const fn = rustSecurity.slice(
    rustSecurity.indexOf("pub fn digest_sql"),
    rustSecurity.indexOf("pub fn digest_sql") + 800
  );
  assert.ok(fn.length > 0, "没找到 digest_sql，解析可能变了");
  assert.ok(
    !/\bsql\.len\(\)/.test(fn),
    "后端又用回字节数了（sql.len()）⇒ 含中文的 SQL 两端摘要必然对不上"
  );
  assert.ok(
    /chars\.len\(\)|chars\(\)\.count\(\)/.test(fn),
    "后端应当按码点数算长度"
  );
});

test("CSV 导入的 canonical 格式没被单方面改过", () => {
  // canonical 是审批绑定的东西：后端改了格式，前端必须跟着改，
  // 否则用户批准的"表 + 行数"与后端算的不是一回事。
  assert.ok(
    /format!\("CSV_IMPORT table=\{\} rows=\{\}", table, rows\)/.test(rustCsv),
    "csv_import.rs 里的 canonical 格式变了 ⇒ 审批绑定的东西变了，需要同步前端与文档"
  );
});

test("后端 csv_import_execute 有门禁参数（不许裸导入）", () => {
  const lib = readFileSync(path.join(repoRoot, "src-tauri/src/lib.rs"), "utf8");
  const fn = lib.slice(
    lib.indexOf("async fn csv_import_execute"),
    lib.indexOf("async fn csv_import_execute") + 1200
  );
  assert.ok(fn.length > 0, "没找到 csv_import_execute");
  assert.ok(fn.includes("gate"), "csv_import_execute 少了 gate ⇒ 导入绕过了审批");

  // 门禁三项住在 ImportGate 结构里（snake_case，serde 语义）
  const gate = lib.slice(lib.indexOf("pub struct ImportGate"), lib.indexOf("pub struct ImportGate") + 600);
  assert.ok(gate.length > 0, "没找到 ImportGate 结构");
  for (const p of ["access_mode", "approval", "expected_rows"]) {
    assert.ok(gate.includes(p), `ImportGate 少了 ${p} ⇒ 导入绕过了审批`);
  }
});
