// 网格里改的单元格 → UPDATE 的纯逻辑回归。
// 跑法：node --experimental-strip-types --test tests/changeCommit.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  buildRowUpdates,
  parseChangeKey,
  parseSingleTableSource,
  quoteIdent,
  sqlLiteral,
} from "../src/ipc/changeCommit.ts";

const cols = [
  { ordinal: 0, name: "id", logical_type: "integer" },
  { ordinal: 1, name: "name", logical_type: "text" },
  { ordinal: 2, name: "score", logical_type: "decimal" },
];
const pks = [{ ordinal: 0, name: "id", logical_type: "integer" }];
const rows = [
  [
    { __kind: "integer", value: 1 },
    { __kind: "text", value: "ann" },
    { __kind: "decimal", value: 3.5 },
  ],
  [
    { __kind: "integer", value: 2 },
    { __kind: "text", value: "bob" },
    { __kind: "null", value: null },
  ],
];

test("changeSet 的键能拆回行列", () => {
  assert.deepEqual(parseChangeKey("12_3"), { rowIndex: 12, ordinal: 3 });
  assert.equal(parseChangeKey("abc"), null);
});

test("单表 SELECT 认得出源表，带别名也行", () => {
  assert.deepEqual(parseSingleTableSource("SELECT * FROM users"), { table: "users", alias: undefined });
  assert.deepEqual(parseSingleTableSource("select id, name from main.users u;"), {
    table: "main.users",
    alias: "u",
  });
});

test("认不出单一行来源的形态一律拒绝，不猜表", () => {
  for (const sql of [
    "SELECT * FROM a JOIN b ON a.id=b.id",
    "SELECT * FROM (SELECT 1) t",
    "SELECT * FROM a, b",
    "SELECT count(*) FROM a",
    "SELECT * FROM a GROUP BY x",
    "UPDATE a SET x = 1",
    "SELECT * FROM a WHERE id IN (SELECT id FROM b)",
    "SELECT DISTINCT x FROM a",
  ]) {
    assert.equal(parseSingleTableSource(sql), null, `应该拒绝：${sql}`);
  }
});

test("注释与行尾分号不影响认表", () => {
  assert.deepEqual(parseSingleTableSource("-- note\nSELECT * FROM t /* x */ ;"), {
    table: "t",
    alias: undefined,
  });
});

test("字符串字面量按方言转义，单引号翻倍", () => {
  assert.equal(sqlLiteral("O'Brien", "text", "sqlite"), "'O''Brien'");
  assert.equal(sqlLiteral("O'Brien", "text", "postgres"), "'O''Brien'");
  assert.equal(sqlLiteral("a\\b", "text", "mysql"), `'a\\\\b'`);
});

test("含 NUL 的值直接抛，不发出去", () => {
  assert.throws(() => sqlLiteral("a\0b", "text", "sqlite"), /NUL/);
});

test("数字列只在值真是数字时才裸写", () => {
  assert.equal(sqlLiteral("42", "integer", "sqlite"), "42");
  assert.equal(sqlLiteral("42abc", "integer", "sqlite"), "'42abc'");
  assert.equal(sqlLiteral("true", "boolean", "sqlite"), "true");
});

test("标识符按方言加引号，带 schema 的逐段加", () => {
  assert.equal(quoteIdent("public.users", "postgres"), '"public"."users"');
  assert.equal(quoteIdent("a b", "mysql"), "`a b`");
});

test("同一行的多格变更合成一条 UPDATE", () => {
  const { statements, rejected } = buildRowUpdates({
    table: "users",
    dialect: "sqlite",
    columns: cols,
    pks,
    rows,
    changes: [
      { rowIndex: 0, ordinal: 1, value: "ann2" },
      { rowIndex: 0, ordinal: 2, value: "9" },
    ],
  });
  assert.equal(rejected.length, 0);
  assert.equal(statements.length, 1);
  assert.equal(
    statements[0].sql,
    `UPDATE "users" SET "name" = 'ann2', "score" = 9 WHERE "id" = 1`
  );
  assert.deepEqual(statements[0].keys, ["0_1", "0_2"]);
});

test("不同行分开成多条，绝不合成没有 WHERE 的语句", () => {
  const { statements } = buildRowUpdates({
    table: "users",
    dialect: "sqlite",
    columns: cols,
    pks,
    rows,
    changes: [
      { rowIndex: 0, ordinal: 1, value: "a" },
      { rowIndex: 1, ordinal: 1, value: "b" },
    ],
  });
  assert.equal(statements.length, 2);
  for (const st of statements) assert.match(st.sql, /WHERE "id" = \d/);
});

test("主键取回是 NULL 或 decode_error 的行单独拒绝，其余照常", () => {
  const rowsWithBadPk = [
    rows[0],
    [
      { __kind: "decode_error", value: null },
      { __kind: "text", value: "x" },
      { __kind: "null", value: null },
    ],
  ];
  const { statements, rejected } = buildRowUpdates({
    table: "users",
    dialect: "sqlite",
    columns: cols,
    pks,
    rows: rowsWithBadPk,
    changes: [
      { rowIndex: 0, ordinal: 1, value: "a" },
      { rowIndex: 1, ordinal: 1, value: "b" },
    ],
  });
  assert.equal(statements.length, 1);
  assert.equal(rejected.length, 1);
  assert.match(rejected[0].reason, /decode_error/);
});

test("结果里已经不存在的行不能提交", () => {
  const { statements, rejected } = buildRowUpdates({
    table: "users",
    dialect: "sqlite",
    columns: cols,
    pks,
    rows: [rows[0]],
    changes: [{ rowIndex: 7, ordinal: 1, value: "a" }],
  });
  assert.equal(statements.length, 0);
  assert.match(rejected[0].reason, /已经不在结果里/);
});

test("列号没有列定义时不拼出半条语句", () => {
  const { statements, rejected } = buildRowUpdates({
    table: "users",
    dialect: "sqlite",
    columns: cols,
    pks,
    rows,
    changes: [{ rowIndex: 0, ordinal: 99, value: "a" }],
  });
  assert.equal(statements.length, 0);
  assert.match(rejected[0].reason, /没有列定义/);
});
