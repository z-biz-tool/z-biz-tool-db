/**
 * SQL 分句规则的回归测试。
 *
 * 跑法：node --experimental-strip-types --test tests/sqlStatements.test.ts
 *
 * 为什么这道门必须存在：朴素的 `split(";")` 会把**字符串里的分号**切开，
 * 而「批量执行」要靠它把编辑器内容拆成一条条交给后端 ——
 * 拆错了，用户点一下批量执行就报语法错，而且完全不知道错在哪。
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { splitStatements, splitStatementTexts, countStatements } from "../src/ipc/sqlStatements.ts";

test("单条语句原样返回", () => {
  assert.deepEqual(splitStatementTexts("SELECT 1"), ["SELECT 1"]);
});

test("多条语句按分号切开", () => {
  assert.deepEqual(splitStatementTexts("SELECT 1; SELECT 2; SELECT 3"), [
    "SELECT 1",
    "SELECT 2",
    "SELECT 3",
  ]);
});

test("结尾多出来的分号不产生空条目", () => {
  assert.deepEqual(splitStatementTexts("SELECT 1;;;"), ["SELECT 1"]);
});

test("字符串里的分号不切开", () => {
  assert.deepEqual(splitStatementTexts("INSERT INTO t VALUES ('a;b')"), [
    "INSERT INTO t VALUES ('a;b')",
  ]);
});

test("SQL 标准的双引号转义（''）不切开", () => {
  assert.deepEqual(splitStatementTexts("INSERT INTO t VALUES ('it''s;fine')"), [
    "INSERT INTO t VALUES ('it''s;fine')",
  ]);
});

test("MySQL 的反斜杠转义不切开", () => {
  assert.deepEqual(splitStatementTexts("INSERT INTO t VALUES ('a\\';b')"), [
    "INSERT INTO t VALUES ('a\\';b')",
  ]);
});

test("双引号与反引号里的分号不切开", () => {
  assert.deepEqual(splitStatementTexts('SELECT "a;b" FROM t'), ['SELECT "a;b" FROM t']);
  assert.deepEqual(splitStatementTexts("SELECT `a;b` FROM t"), ["SELECT `a;b` FROM t"]);
});

test("行注释里的分号不切开", () => {
  assert.deepEqual(splitStatementTexts("SELECT 1 -- 这里有个 ; 号\n; SELECT 2"), [
    "SELECT 1 -- 这里有个 ; 号",
    "SELECT 2",
  ]);
});

test("MySQL 的 # 注释同样不算", () => {
  assert.deepEqual(splitStatementTexts("SELECT 1 # ; 号\n; SELECT 2"), ["SELECT 1 # ; 号", "SELECT 2"]);
});

test("块注释里的分号不切开，且注释块本身不产生条目", () => {
  // 注释**原样保留**（界面上给用户看的、审批算摘要的、发给数据库执行的必须是同一份文本），
  // 但注释里的分号不构成语句边界。
  assert.deepEqual(splitStatementTexts("/* a; b */ SELECT 1"), ["/* a; b */ SELECT 1"]);
  // 只有块注释 ⇒ 不产生条目
  assert.deepEqual(splitStatementTexts("/* a; b */"), []);
});

test("注释原样保留：切出来的文本是原文的一段逐字拷贝", () => {
  // 这条是「审批摘要 = 执行文本」的底座：切出来的每条都要能在原文里原样找到。
  const sql = "/* 头 */ SELECT 1 -- 尾\n; SELECT 2";
  for (const part of splitStatements(sql)) {
    assert.ok(
      sql.includes(part.text),
      `切出来的「${part.text}」不是原文的逐字片段`
    );
  }
});

test("纯注释不产生条目（不该为一条注释去要审批）", () => {
  assert.deepEqual(splitStatementTexts("-- 只有注释"), []);
  assert.deepEqual(splitStatementTexts("/* 只有注释 */"), []);
  assert.deepEqual(splitStatementTexts("   \n  \n "), []);
});

test("分号在字符串里、语句在字符串外：分得对", () => {
  assert.deepEqual(splitStatementTexts("SELECT 'x;y'; SELECT 2"), ["SELECT 'x;y'", "SELECT 2"]);
});

test("偏移量指向原文里的起始位置", () => {
  const sql = "SELECT 1;\nSELECT 2;";
  const parts = splitStatements(sql);
  assert.equal(parts.length, 2);
  assert.equal(sql.slice(parts[0].start, parts[0].start + parts[0].text.length), "SELECT 1");
  assert.equal(sql.slice(parts[1].start, parts[1].start + parts[1].text.length), "SELECT 2");
});

test("count 出来的条数与切出来的一致", () => {
  const sql = "SELECT 1; SELECT 'a;b'; -- x\n; SELECT 3";
  assert.equal(countStatements(sql), splitStatements(sql).length);
  assert.equal(countStatements(sql), 3);
});

test("未闭合的引号不吞掉后面的语句（宁可报错也别静默）", () => {
  // 未闭合的引号：按「一直吃到字符串结束」处理，结果是 1 条（而不是切出 2 条）
  // —— 这与后端 classify 的行为一致：无法判定时宁可当成一条交给后端去报错。
  assert.equal(countStatements("SELECT 'abc"), 1);
});
