// T-052：错误结构化显示
//
// 后端 ApiError 在 S0/S1 阶段还未统一为 ApiError 结构体；
// 我们用 client 端模式匹配识别现有错误码（如只读拦截、审批缺失、密码剥除、
// 端口非法、Generation 过期），并对其他错误做轻量格式化。
//
// 这样既能识别当前已知错误，又能在 ApiError 上线后自动透传。

export type ErrorCategory =
  | "authz"
  | "readonly_blocked"
  | "approval"
  | "stale_generation"
  | "invalid_port"
  | "secret_stripped"
  | "connection"
  | "sql_classify"
  | "ipc"
  | "unknown";

export interface StructuredError {
  category: ErrorCategory;
  code: string;
  retryable: boolean;
  stage?: string;
  safe_message: string;
  raw: string;
}

const KNOWN_PATTERNS: Array<{ category: ErrorCategory; pattern: RegExp; code: string; retryable: boolean }> = [
  { category: "authz",            pattern: /AUTHZ|认证|授权|无权/,                            code: "AUTHZ_DENIED",         retryable: false },
  { category: "readonly_blocked", pattern: /只读模式拒绝/,                                    code: "READONLY_BLOCKED",     retryable: false },
  { category: "approval",         pattern: /审批校验失败|审批缺失|审批.*过期|审批.*摘要/,        code: "APPROVAL_DENIED",      retryable: false },
  { category: "stale_generation", pattern: /stale|代次|generation|期望代次/,                  code: "STALE_GENERATION",     retryable: true  },
  { category: "invalid_port",     pattern: /端口.*非法|端口.*超过|端口字段|端口字符串/,          code: "INVALID_PORT",         retryable: false },
  { category: "secret_stripped",  pattern: /密码.*清空|password.*clear|strip/,                  code: "SECRET_STRIPPED",      retryable: false },
  { category: "sql_classify",     pattern: /kind=|safety=|静态分类/,                          code: "SQL_CLASSIFY",         retryable: false },
  { category: "connection",       pattern: /连接超时|connect|network|timeout|握手|TLS/,        code: "CONNECTION_FAILED",    retryable: true  },
  { category: "ipc",              pattern: /IPC|invoke|invoke|tauri/,                          code: "IPC_ERROR",            retryable: true  },
];

export function classifyError(raw: string): StructuredError {
  for (const k of KNOWN_PATTERNS) {
    if (k.pattern.test(raw)) {
      return {
        category: k.category,
        code: k.code,
        retryable: k.retryable,
        safe_message: raw.length > 240 ? raw.slice(0, 240) + "…" : raw,
        raw,
      };
    }
  }
  return {
    category: "unknown",
    code: "UNKNOWN",
    retryable: true,
    safe_message: raw.length > 240 ? raw.slice(0, 240) + "…" : raw,
    raw,
  };
}

export function errorDescription(err: StructuredError): string {
  switch (err.category) {
    case "readonly_blocked":
      return "当前为只读模式。打开「可写」开关并通过审批后即可写入。";
    case "approval":
      return "写入需要有效审批。请重新生成审批 token 后再试。";
    case "stale_generation":
      return "结果已过期（连接/会话/标签已切换）。请重试查询。";
    case "invalid_port":
      return "端口字段非法（u16 范围 1-65535，且不可猜测 IPv6 端口）。";
    case "secret_stripped":
      return "出于安全，密码字段在落盘前被清空。如需新密码请重新输入。";
    case "authz":
      return "权限不足，请联系数据库管理员或切换账户。";
    case "connection":
      return "网络/凭据/握手问题。请检查 VPN、防火墙与主机端口。";
    case "sql_classify":
      return "SQL 静态分类器拒绝该语句。请人工确认语法并改写。";
    case "ipc":
      return "前后端通信异常，可重试一次。";
    default:
      return err.retryable ? "未知错误，可重试一次。" : "未知错误，不可重试。";
  }
}
