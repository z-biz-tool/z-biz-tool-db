// Agent 类型定义
/** 对话意图。只收"用户这句本身就是输入"的两种：
 *  query = 自然语言 → SQL，diagnose = 报错原文 → 原因与改法。
 *  解释/优化/分析结果那三条的输入是编辑器里的 SQL 与结果集，
 *  放在 AI 助手弹窗的按钮上更合适，进对话栏只会让用户以为打字有用。 */
export type AgentIntent = 'query' | 'diagnose';

export interface AgentMessage {
  id: string;
  /** system 是"发生过的事"（例如一条已执行的语句），不是用户打的话，别画成用户气泡 */
  role: 'user' | 'agent' | 'system';
  content: string;
  sql?: string;
  /** 带这个字段的 system 行是一次失败现场：面板据此给出「诊断这条报错」 */
  error?: string;
  timestamp: number;
}

/** 一轮提问能附带的最小上下文。
 *  sql = 这句话是从哪条语句的报错里来的。不传就按编辑器当前那段走——
 *  报错发生在用户改稿之前时，只有带着当时那条才能问对（T-075）。 */
export interface AgentAskContext {
  sql?: string;
}

export interface AgentResponse {
  success: boolean;
  content: string;
  sql?: string;
  error?: string;
}
