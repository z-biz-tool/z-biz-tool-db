// T-045 兼容层：审批 Modal 状态与生产逻辑
// 与 src-tauri/src/security.rs 配对；本文件保持前端无任何 Tauri/Mock 副作用。

import { digestSql } from "./db";
import type { ApprovalGrant } from "./db";

export type { ApprovalGrant };

export interface PendingApproval {
  sql: string;
  environment: string;
  issuedAt: number;
  ttlSec: number;
}

export function buildGrant(p: PendingApproval): ApprovalGrant {
  return {
    approval_id: makeUuidV4(),
    sql_digest: digestSql(p.sql),
    environment: p.environment,
    issued_at: p.issuedAt,
    expires_at: p.issuedAt + p.ttlSec,
    consumed: false,
  };
}

// 极简 UUID v4 生成（足够防碰撞，未做加密随机性保证）
export function makeUuidV4(): string {
  const bytes = new Uint8Array(16);
  if (typeof crypto !== "undefined" && crypto.getRandomValues) {
    crypto.getRandomValues(bytes);
  } else {
    for (let i = 0; i < 16; i++) bytes[i] = Math.floor(Math.random() * 256);
  }
  // v4 标识位 + variant
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const h: string[] = [];
  for (let i = 0; i < 16; i++) h.push(bytes[i].toString(16).padStart(2, "0"));
  return `${h.slice(0, 4).join("")}-${h.slice(4, 6).join("")}-${h.slice(6, 8).join("")}-${h.slice(8, 10).join("")}-${h
    .slice(10, 16)
    .join("")}`;
}

// 极简 SQL 分类器：与后端 db::sql_classify 保持同步词法边界
// 仅用于前端在执行前判断是否需要审批（与后端校验最终一致）
export function isLikelyWriteSql(sql: string): boolean {
  // 剥字符串与注释后看首关键字
  const stripped = stripStringsAndComments(sql).trimStart();
  // 取首 token
  const first = stripped.split(/\s|\(|\;/)[0]?.toUpperCase() ?? "";
  if (
    first === "INSERT" ||
    first === "UPDATE" ||
    first === "DELETE" ||
    first === "REPLACE" ||
    first === "MERGE" ||
    first === "CREATE" ||
    first === "DROP" ||
    first === "ALTER" ||
    first === "TRUNCATE" ||
    first === "RENAME" ||
    first === "GRANT" ||
    first === "REVOKE" ||
    first === "BEGIN" ||
    first === "COMMIT" ||
    first === "ROLLBACK" ||
    first === "SAVEPOINT" ||
    first === "SET"
  ) {
    return true;
  }
  // SELECT FOR UPDATE / SELECT FOR SHARE 视为带锁只读，应拒绝只读通道
  const upper = stripped.toUpperCase();
  if (
    (first === "SELECT" || first === "WITH") &&
    (upper.includes("FOR UPDATE") || upper.includes("FOR SHARE"))
  ) {
    return true;
  }
  return false;
}

function stripStringsAndComments(sql: string): string {
  let out = "";
  let i = 0;
  while (i < sql.length) {
    const c = sql[i];
    // 行注释
    if (c === "-" && sql[i + 1] === "-") {
      while (i < sql.length && sql[i] !== "\n") i++;
      continue;
    }
    // 块注释
    if (c === "/" && sql[i + 1] === "*") {
      i += 2;
      while (i < sql.length && !(sql[i] === "*" && sql[i + 1] === "/")) i++;
      i = Math.min(i + 2, sql.length);
      continue;
    }
    // 单引号字符串
    if (c === "'") {
      out += "'";
      i++;
      while (i < sql.length) {
        if (sql[i] === "'") {
          if (sql[i + 1] === "'") {
            out += "''";
            i += 2;
            continue;
          }
          out += "'";
          i++;
          break;
        }
        out += sql[i];
        i++;
      }
      continue;
    }
    // 双引号
    if (c === '"') {
      out += '"';
      i++;
      while (i < sql.length) {
        if (sql[i] === '"') {
          if (sql[i + 1] === '"') {
            out += '""';
            i += 2;
            continue;
          }
          out += '"';
          i++;
          break;
        }
        out += sql[i];
        i++;
      }
      continue;
    }
    out += c;
    i++;
  }
  return out;
}
