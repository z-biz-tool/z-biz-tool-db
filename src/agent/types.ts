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
  timestamp: number;
}

export interface AgentResponse {
  success: boolean;
  content: string;
  sql?: string;
  error?: string;
}
