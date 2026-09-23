// Agent 类型定义
/** 对话意图。只收"用户这句本身就是输入"的三种：
 *  query = 自然语言 → SQL，diagnose = 报错原文 → 原因与改法，
 *  report = 自然语言 → 一张跨库报表（链子在报表工作台那边跑：挑表→起草→取数→出图）。
 *  解释/优化/分析结果那三条的输入是编辑器里的 SQL 与结果集，
 *  放在 AI 助手弹窗的按钮上更合适，进对话栏只会让用户以为打字有用。 */
export type AgentIntent = 'query' | 'diagnose' | 'report';

/** 本机挡下的表其实躺在别的连接里：面板据此给出「拿去 AI 报表出图」，
 *  question 是当初那句话，交接过去不用用户再抄一遍。 */
export interface AgentCrossDb {
  question: string;
  tables: string[];
  connections: string[];
}

export interface AgentMessage {
  id: string;
  /** system 是"发生过的事"（例如一条已执行的语句），不是用户打的话，别画成用户气泡 */
  role: 'user' | 'agent' | 'system';
  content: string;
  sql?: string;
  /** 带这个字段的 system 行是一次失败现场：面板据此给出「诊断这条报错」 */
  error?: string;
  cross?: AgentCrossDb;
  /** 带这个字段的 agent 气泡是一次被本机挡下的稿子：面板据此给出「照这条错误改」 */
  fix?: SqlFixTicket;
  timestamp: number;
}

/** 一键改稿要带的两半：拒因 + 被拒的那条 SQL。
 *  模型是单发的，下一轮看不见自己刚写的那条，缺任何一半都是在凭空重写。 */
export interface SqlFixTicket {
  question: string;
  error: string;
  sql: string;
}

/** 一轮提问能附带的最小上下文。
 *  sql = 这句话是从哪条语句的报错里来的。不传就按编辑器当前那段走——
 *  报错发生在用户改稿之前时，只有带着当时那条才能问对（T-075）。
 *  fix = 这一轮是「照着上一次的拒因改」，而不是重新问一遍。 */
export interface AgentAskContext {
  sql?: string;
  fix?: SqlFixTicket;
}

export interface AgentResponse {
  success: boolean;
  content: string;
  sql?: string;
  error?: string;
  cross?: AgentCrossDb;
  /** 这一轮没过本机校验、且手上确有被拒的那条 SQL 时带回来 */
  fix?: SqlFixTicket;
}
