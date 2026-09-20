// Agent 类型定义
export interface AgentConfig {
  id: string;
  name: string;
  description: string;
  provider: 'local' | 'api';
  apiUrl?: string;
  model?: string;
}

export interface AgentMessage {
  id: string;
  role: 'user' | 'agent' | 'system';
  content: string;
  sql?: string;
  timestamp: number;
}

export interface AgentContext {
  connectionId: string;
  databaseType: string;
  databaseName: string;
  tables?: Array<{
    name: string;
    columns: Array<{ name: string; type: string }>;
  }>;
}

export interface AgentResponse {
  success: boolean;
  content: string;
  sql?: string;
  context?: AgentContext;
  error?: string;
}
