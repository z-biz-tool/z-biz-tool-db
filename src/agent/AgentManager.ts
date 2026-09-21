import { create } from 'zustand';
import { AgentConfig, AgentMessage, AgentContext, AgentResponse } from './types';

interface AgentStore {
  config: AgentConfig;
  messages: AgentMessage[];
  context: AgentContext;
  addMessage: (message: AgentMessage) => void;
  setContext: (context: AgentContext) => void;
  query: (naturalLanguage: string) => Promise<AgentResponse>;
  optimize: (sql: string) => Promise<AgentResponse>;
  analyze: (sql: string, results: any) => Promise<AgentResponse>;
  diagnoseError: (error: string, sql: string) => Promise<AgentResponse>;
}

/**
 * T-005：S0 止血。所有 Agent 路径均尚未接通真实服务；任何未接通的方法
 * 都直接返回"服务未配置"错误，禁止伪造 SQL 或示例响应。
 * 后端 agent_* 命令也已去掉静默重执行 SQL 行为（DB-04 / A08）。
 */
const NOT_CONFIGURED: AgentResponse = {
  success: false,
  content: '',
  error: 'Agent 服务未配置；S0 阶段所有路径均不可用。请前往设置配置后端服务后再试。',
};

export const useAgentStore = create<AgentStore>((set) => ({
  config: {
    id: 'default-agent',
    name: 'SQL Agent',
    description: '数据库查询助手',
    provider: 'local',
  },
  messages: [],
  context: {
    connectionId: '',
    databaseType: '',
    databaseName: '',
  },

  addMessage: (message) => set((state) => ({
    messages: [...state.messages, message],
  })),

  setContext: (context) => set({ context }),

  query: async (_naturalLanguage) => {
    return NOT_CONFIGURED;
  },

  optimize: async (_sql) => {
    return NOT_CONFIGURED;
  },

  // T-004：结果分析仅接 resultData，禁止再次执行 SQL。
  analyze: async (_sql, _results) => {
    return NOT_CONFIGURED;
  },

  diagnoseError: async (_error, _sql) => {
    return NOT_CONFIGURED;
  },
}));
