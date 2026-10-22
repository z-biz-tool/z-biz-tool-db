/**
 * 把一段 SQL 切成一条条独立的语句。
 *
 * 为什么需要它：
 * - 「批量执行」要把编辑器里的多条语句拆开，逐条交给后端的 `execute_batch`
 *   （后端**拒绝**多语句单次执行 —— 那是审批/只读门禁的防绕过设计）。
 * - 朴素的 `sql.split(";")` 会把**字符串里的分号**也切开：
 *   `INSERT INTO t VALUES ('a;b')` 会被切成两半 ⇒ 语法错误 ⇒ 用户一脸莫名其妙。
 *
 * 两条口径（都是踩过才知道的）：
 * 1. **注释原样保留**。界面上给用户看的、交给审批摘要算摘要的、发给数据库执行的，
 *    必须是**同一份文本** —— 哪怕只是少了一句注释，三者对不上就会出现
 *    「审批的是 A、执行的是 B」。所以这里只把注释里的分号当成分号的一部分，
 *    不把注释从语句里抠掉。
 * 2. **偏移量必须精确**：切出来的每条都要能指回原文的位置（界面据此高亮）。
 *    为此 buf 里**原样保留**注释（长度不变），否则一去注释，偏移就全错了。
 *
 * 分句规则（与后端 `db::sql_classify` 的分句规则保持一致）：
 * - 单引号串：`''` 是转义（SQL 标准），`\'` 与 `\\` 也按转义处理（MySQL 习惯）；
 * - 双引号与反引号：当成带引号的标识符（MySQL/PG 都会这么用）；
 * - `--` 到行尾、`#` 到行尾（MySQL）、斜杠星号块注释：注释里的分号不算；
 * - 分号**不进**结果（后端对单条语句再加分号会被多语句计数看见）。
 */

/** 一条语句在结果里的形态 */
export interface SplitResult {
  /** 去掉首尾空白、去掉结尾分号后的语句正文（注释原样保留） */
  text: string;
  /** 在原文里的起始偏移（便于界面高亮「这条对应原文哪一段」） */
  start: number;
}

/**
 * 切分 SQL。返回的每条都已去掉首尾空白与结尾分号。
 *
 * 纯注释 / 纯空白 / 末尾多出来的分号**不会**产生条目 ——
 * 「批量执行」不该为一条注释去问用户要不要审批。
 */
export function splitStatements(sql: string): SplitResult[] {
  const out: SplitResult[] = [];
  let buf = "";
  /** 这一段里有没有真正的代码（排除注释与空白） */
  let hasCode = false;
  /** 这一段在原文里的起点 */
  let stmtStart = 0;
  let i = 0;
  const n = sql.length;

  const flush = () => {
    const trimmed = buf.trim();
    if (trimmed && hasCode) {
      // buf 是原文的**逐字拷贝**（注释也原样留着）⇒ 长度对得上 ⇒ 偏移量精确
      const lead = buf.length - buf.trimStart().length;
      out.push({ text: trimmed, start: stmtStart + lead });
    }
    buf = "";
    hasCode = false;
  };

  while (i < n) {
    const c = sql[i];
    const c2 = sql.slice(i, i + 2);

    // 行注释：-- 与 #（MySQL）。原样保留，但不算「代码」。
    if (c2 === "--" || c === "#") {
      const end = sql.indexOf("\n", i);
      const stop = end < 0 ? n : end;
      buf += sql.slice(i, stop);
      i = stop;
      continue;
    }
    // 块注释。原样保留，但不算「代码」。
    if (c2 === "/*") {
      const end = sql.indexOf("*/", i + 2);
      const stop = end < 0 ? n : end + 2;
      buf += sql.slice(i, stop);
      i = stop;
      continue;
    }
    // 带引号的段：单引号 / 双引号 / 反引号
    if (c === "'" || c === '"' || c === "`") {
      const quote = c;
      buf += c;
      i++;
      while (i < n) {
        if (sql[i] === "\\" && quote === "'" && i + 1 < n) {
          // MySQL 的反斜杠转义：\ ' \\ 都要原样带走
          buf += sql[i] + sql[i + 1];
          i += 2;
          hasCode = true;
          continue;
        }
        if (sql[i] === quote) {
          // 连续两个引号 = 转义（两个单引号或两个双引号），不是结束
          if (sql[i + 1] === quote) {
            buf += quote + quote;
            i += 2;
            hasCode = true;
            continue;
          }
          buf += quote;
          i++;
          hasCode = true;
          break;
        }
        buf += sql[i];
        i++;
        hasCode = true;
      }
      continue;
    }
    // 分号 = 一条语句结束（分号本身不进结果）
    if (c === ";") {
      flush();
      i++;
      stmtStart = i;
      continue;
    }
    if (!/\s/.test(c)) hasCode = true;
    buf += c;
    i++;
  }
  flush();
  return out;
}

/** 只要语句正文（不要偏移） */
export function splitStatementTexts(sql: string): string[] {
  return splitStatements(sql).map((s) => s.text);
}

/**
 * 这段 SQL 里有没有**多于一条**语句。
 *
 * 用途：界面上「批量执行」按钮该显示几条，让用户在做之前就知道会发生什么，
 * 而不是点了才弹一句「共 3 条」。
 */
export function countStatements(sql: string): number {
  return splitStatements(sql).length;
}
