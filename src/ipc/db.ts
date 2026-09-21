// T-019 前端 v2 IPC 契约
//
// 与后端 src-tauri/src/db/ipc.rs 配合；TypeScript 仅描述结构，
// 不参与运行——主链路仍走原 invoke 命令；新模块为后续平滑迁移做准备。

export interface ApiError {
  code: string;
  stage: string;
  retryable: boolean;
  query_id?: string;
  transaction_state?: string;
  safe_message: string;
}

export interface V2Envelope<T> {
  kind: "ok" | "err";
  data?: T;
  query_id?: string;
  error?: ApiError;
}

/** 业务错误码常量；与后端 db::ipc 保持一致 */
export const ERR_CODES = {
  AuthzDenied: "AUTHZ_DENIED",
  ReadonlyBlocked: "READONLY_BLOCKED",
  ApprovalDenied: "APPROVAL_DENIED",
  StaleGeneration: "STALE_GENERATION",
  InvalidPort: "INVALID_PORT",
} as const;

/** 前端识别：授权决策结果 */
export interface ApprovalGrant {
  approval_id: string;
  sql_digest: string;
  environment: string;
  issued_at: number;
  expires_at: number;
  consumed: boolean;
}

/** 简单的 sql 摘要，与后端 `security::digest_sql` 保持一致 */
export function digestSql(sql: string): string {
  const head = Array.from(sql).slice(0, 64).join("");
  return `len=${sql.length}|head=${head}`;
}

/** 检查审批是否仍可用 */
export function isApprovalUsable(
  grant: ApprovalGrant | null | undefined,
  sql: string,
  nowTs: number,
): { ok: boolean; reason?: string } {
  if (!grant) return { ok: false, reason: "审批缺失" };
  if (grant.consumed) return { ok: false, reason: "审批已被使用" };
  if (grant.expires_at < nowTs) return { ok: false, reason: "审批已过期" };
  if (grant.sql_digest !== digestSql(sql))
    return { ok: false, reason: "审批不匹配当前 SQL" };
  return { ok: true };
}

export type CancelState =
  | "not_found"
  | "accepted"
  | "confirmed"
  | "pending"
  | "unknown";

export interface CancelResponse {
  query_id: string;
  state: CancelState;
  message: string;
}
