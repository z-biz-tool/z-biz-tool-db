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

export const useAgentStore = create<AgentStore>((set, get) => ({
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

  query: async (naturalLanguage) => {
    const { config, context } = get();
    
    try {
      // TODO: 调用 Agent 服务
      const response: AgentResponse = {
        success: true,
        content: `我将为您查询: ${naturalLanguage}`,
        sql: `SELECT * FROM users WHERE name LIKE '%${naturalLanguage}%'`,
        context,
      };
      
      get().addMessage({
        id: Date.now().toString(),
        role: 'user',
        content: naturalLanguage,
        timestamp: Date.now(),
      });
      
      get().addMessage({
        id: (Date.now() + 1).toString(),
        role: 'agent',
        content: response.content,
        sql: response.sql,
        timestamp: Date.now(),
      });
      
      return response;
    } catch (error) {
      return {
        success: false,
        content: '',
        error: error instanceof Error ? error.message : 'Unknown error',
      };
    }
  },

  optimize: async (sql) => {
    const { config, context } = get();
    
    try {
      // TODO: 调用 Agent 服务
      const response: AgentResponse = {
        success: true,
        content: 'SQL 优化建议',
        sql: sql.toUpperCase(), // 简单示例
        context,
      };
      
      get().addMessage({
        id: Date.now().toString(),
        role: 'user',
        content: `优化 SQL: ${sql}`,
        timestamp: Date.now(),
      });
      
      get().addMessage({
        id: (Date.now() + 1).toString(),
        role: 'agent',
        content: response.content,
        sql: response.sql,
        timestamp: Date.now(),
      });
      
      return response;
    } catch (error) {
      return {
        success: false,
        content: '',
        error: error instanceof Error ? error.message : 'Unknown error',
      };
    }
  },

  analyze: async (sql, results) => {
    const { config, context } = get();
    
    try {
      // TODO: 调用 Agent 服务
      const response: AgentResponse = {
        success: true,
        content: '查询结果分析',
        sql,
        context,
      };
      
      get().addMessage({
        id: Date.now().toString(),
        role: 'user',
        content: `分析结果: ${sql}`,
        timestamp: Date.now(),
      });
      
      get().addMessage({
        id: (Date.now() + 1).toString(),
        role: 'agent',
        content: response.content,
        sql: response.sql,
        timestamp: Date.now(),
      });
      
      return response;
    } catch (error) {
      return {
        success: false,
        content: '',
        error: error instanceof Error ? error.message : 'Unknown error',
      };
    }
  },

  diagnoseError: async (error, sql) => {
    const { config, context } = get();
    
    try {
      // TODO: 调用 Agent 服务
      const response: AgentResponse = {
        success: true,
        content: `错误诊断: ${error}`,
        sql,
        context,
      };
      
      get().addMessage({
        id: Date.now().toString(),
        role: 'user',
        content: `错误: ${error}\nSQL: ${sql}`,
        timestamp: Date.now(),
      });
      
      get().addMessage({
        id: (Date.now() + 1).toString(),
        role: 'agent',
        content: response.content,
        sql: response.sql,
        timestamp: Date.now(),
      });
      
      return response;
    } catch (error) {
      return {
        success: false,
        content: '',
        error: error instanceof Error ? error.message : 'Unknown error',
      };
    }
  },
}));
