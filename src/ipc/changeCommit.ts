// 网格单元格的变更 → 可执行的 UPDATE 语句。
//
// 这一层刻意做成纯函数：结果网格里的每一格只是「某张表某一行的某一列」，
// 但界面拿到的是 TaggedCell 二维数组，行身份不在数据里。要把改动写回库，
// 必须凑齐三样：单表 SELECT 的表名、这张表的主键列、这一行主键的当前值。
// 少一样都不能提交 —— 宁可拒绝，也不能发一条没有 WHERE 的 UPDATE。

export interface ColumnLike {
  ordinal: number;
  name: string;
  logical_type?: string;
}

export interface CellChange {
  rowIndex: number;
  ordinal: number;
  value: string;
}

export type Dialect = "sqlite" | "mysql" | "postgres" | string;

/** changeSet 的键是 `${rowKey}_${ordinal}`（见 App.tsx 的 cellKey） */
export function parseChangeKey(key: string): { rowIndex: number; ordinal: number } | null {
  const i = key.lastIndexOf("_");
  if (i < 0) return null;
  const rowIndex = Number(key.slice(0, i));
  const ordinal = Number(key.slice(i + 1));
  return Number.isInteger(rowIndex) && Number.isInteger(ordinal) ? { rowIndex, ordinal } : null;
}

/**
 * 从一条 SELECT 里认出唯一的源表；认不出就返回 null（不猜）。
 *
 * 只接受 `FROM <单表>[别名]` 且没有 JOIN / 子查询 / 逗号多表 / GROUP BY / DISTINCT /
 * 聚合的形态 —— 这些情况下界面里的行不再对应一张表的一行，反推主键必然写错别的行。
 */
export function parseSingleTableSource(sql: string): { table: string; alias?: string } | null {
  const text = sql
    .replace(/--[^\n]*$/gm, "")
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .trim()
    .replace(/;\s*$/, "");
  if (!/^select\b/i.test(text)) return null;
  if (/\b(join|union|with|group\s+by|having|distinct|for\s+update)\b/i.test(text)) return null;
  if (/\(\s*select\b/i.test(text)) return null;
  if (/\bcount\s*\(|\bsum\s*\(|\bavg\s*\(|\bmin\s*\(|\bmax\s*\(/i.test(text)) return null;

  const m = /\bfrom\s+([`"\[]?)([A-Za-z_][\w$]*(?:\.[A-Za-z_][\w$]*)?)\1(\s+(?:as\s+)?([A-Za-z_]\w*))?/i.exec(
    text
  );
  if (!m) return null;
  // FROM 之后紧跟逗号 = 多表笛卡尔积，同样不能定位行
  const afterFrom = text.slice(m.index + m[0].length);
  if (/^\s*,/.test(afterFrom)) return null;
  const rest = text.slice(text.toLowerCase().indexOf("from") + 4);
  if (/\bfrom\b/i.test(rest)) return null;
  return { table: m[2], alias: m[4] };
}

/** 标识符按方言加引号；库名/表名带点号时逐段加 */
export function quoteIdent(name: string, dialect: Dialect): string {
  const wrap = (seg: string) =>
    dialect === "mysql" ? `\`${seg.replace(/`/g, "``")}\`` : `"${seg.replace(/"/g, '""')}"`;
  return name.split(".").map(wrap).join(".");
}

/**
 * 值字面量。数字/布尔列在值确实是那个形状时才裸写，否则一律走字符串字面量，
 * 因为网格里的编辑框给到的永远是 string，把 '42abc' 当数字发出去是坏数据。
 */
export function sqlLiteral(
  raw: string,
  logicalType: string | undefined,
  dialect: Dialect
): string {
  if (raw.includes("\0")) throw new Error("值里含 NUL，不能写进 SQL 字面量");
  const t = (logicalType || "").toLowerCase();
  if (/^(int|integer|bigint|smallint|mediumint|tinyint|numeric|decimal|float|double|real|number)/.test(t)) {
    if (/^-?\d+(\.\d+)?$/.test(raw.trim())) return raw.trim();
  }
  if (/^(bool|boolean)/.test(t) && /^(true|false)$/i.test(raw.trim())) {
    return raw.trim().toLowerCase();
  }
  // MySQL 默认把反斜杠当转义起始，先 doubling 再处理引号，否则 \' 会被吃掉
  const body = dialect === "mysql" ? raw.replace(/\\/g, "\\\\").replace(/'/g, "\\'") : raw.replace(/'/g, "''");
  return `'${body}'`;
}

export interface RowIdentity {
  rowIndex: number;
  pkCells: { column: string; literal: string }[];
}

export interface BuiltStatement {
  sql: string;
  rowIndexes: number[];
  keys: string[];
}

/**
 * 按行把变更打包成 UPDATE。每行一条语句，一条语句里带上这一行被改的全部列。
 *
 * 返回 rejected 而不是抛错：一屏里可能只有某几行定位不了（主键取回失败），
 * 那几行要单独告知并留在待提交区，而不是整批不动。
 */
export function buildRowUpdates(opts: {
  table: string;
  dialect: Dialect;
  columns: ColumnLike[];
  pks: { ordinal: number; name: string; logical_type?: string }[];
  rows: any[][];
  changes: CellChange[];
}): { statements: BuiltStatement[]; rejected: { change: CellChange; reason: string }[] } {
  const { table, dialect, columns, pks, rows, changes } = opts;
  const colByOrdinal = new Map(columns.map((c) => [c.ordinal, c]));
  const statements: BuiltStatement[] = [];
  const rejected: { change: CellChange; reason: string }[] = [];

  const byRow = new Map<number, CellChange[]>();
  for (const ch of changes) {
    if (!colByOrdinal.has(ch.ordinal)) {
      rejected.push({ change: ch, reason: `第 ${ch.ordinal} 列没有列定义，无法确定要写哪一列` });
      continue;
    }
    const list = byRow.get(ch.rowIndex) || [];
    list.push(ch);
    byRow.set(ch.rowIndex, list);
  }

  for (const [rowIndex, rowChanges] of byRow) {
    const row = rows[rowIndex];
    if (!row) {
      rejected.push(...rowChanges.map((change) => ({ change, reason: "这一行已经不在结果里了" })));
      continue;
    }
    const where: string[] = [];
    let idFail = "";
    for (const pk of pks) {
      const cell = row[pk.ordinal];
      if (!cell || cell.__kind === "null") {
        // 主键为空 = 定位不了唯一行（NULL 不能这么比），也可能是取回时就坏了
        idFail = `主键列 ${pk.name} 在本行是 NULL，无法唯一定位`;
        break;
      }
      if (cell.__kind === "decode_error" || cell.__kind === "unsupported") {
        idFail = `主键列 ${pk.name} 取回的是 ${cell.__kind}，值不可信`;
        break;
      }
      const value = String(cell.value);
      where.push(`${quoteIdent(pk.name, dialect)} = ${sqlLiteral(value, pk.logical_type, dialect)}`);
    }
    if (idFail) {
      rejected.push(...rowChanges.map((change) => ({ change, reason: idFail })));
      continue;
    }

    const setParts = rowChanges.map((ch) => {
      const col = colByOrdinal.get(ch.ordinal)!;
      return `${quoteIdent(col.name, dialect)} = ${sqlLiteral(ch.value, col.logical_type, dialect)}`;
    });
    statements.push({
      sql: `UPDATE ${quoteIdent(table, dialect)} SET ${setParts.join(", ")} WHERE ${where.join(" AND ")}`,
      rowIndexes: [rowIndex],
      keys: rowChanges.map((ch) => `${ch.rowIndex}_${ch.ordinal}`),
    });
  }

  return { statements, rejected };
}
