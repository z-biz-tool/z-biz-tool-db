// Agent 交互面板 - 共享组件
// 所有 z-biz-tool 项目都可以使用
//
// 面板本身不认识任何后端命令：一轮对话走 useAgentStore.ask，
// 真实实现由宿主应用用 setHandler 登记（z-biz-tool-db 里是 App.tsx）。
// 没登记时 ask 会照实回"没有可用的后端实现"，不会伪造回答。

import { useState, useEffect, useRef } from 'react';
import {
  Alert,
  Button,
  Card,
  Input,
  List,
  Segmented,
  Space,
  Spin,
  Tooltip,
  Typography,
} from 'antd';
import { RobotOutlined, SendOutlined } from '@ant-design/icons';
import { useAgentStore } from '../agent/AgentManager';
import type { AgentIntent } from '../agent/types';

const { TextArea } = Input;
const { Text } = Typography;

const INTENT_PLACEHOLDER: Record<AgentIntent, string> = {
  query: '想要查什么？例如：按城市统计已支付订单金额（模型只能在已勾选的表的真实字段里挑）',
  diagnose: '把报错原文贴进来，SQL 用编辑器里当前那段',
};

/** 失败现场里的 SQL / 报错原文：等宽、能滚、不许把面板撑破 */
const RAW_BOX: React.CSSProperties = {
  backgroundColor: 'rgba(0, 0, 0, 0.05)',
  padding: '4px 8px',
  borderRadius: 4,
  fontFamily: 'monospace',
  fontSize: 12,
  whiteSpace: 'pre-wrap',
  wordBreak: 'break-all',
  maxHeight: 120,
  overflowY: 'auto',
  marginTop: 4,
};

export interface AgentPanelProps {
  /** 这轮对话依据的上下文（连的哪个库、用哪些表），由宿主拼好；空则不显示 */
  hint?: string;
  /** 生成出的 SQL 落到哪里。不给就不显示「填进编辑器」 */
  onUseSql?: (sql: string) => void;
  /** 清空对话时的额外收尾：会话都没了，宿主据以改稿的"上一稿"也该作废 */
  onClear?: () => void;
}

export function AgentPanel({ hint, onUseSql, onClear }: AgentPanelProps) {
  const [input, setInput] = useState('');
  const [intent, setIntent] = useState<AgentIntent>('query');
  const { messages, busy, ask, clear } = useAgentStore();
  const messagesEndRef = useRef<HTMLDivElement>(null);

  const scrollToBottom = () => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  };

  useEffect(() => {
    scrollToBottom();
  }, [messages, busy]);

  const send = () => {
    const body = input.trim();
    if (!body || busy) return;
    setInput('');
    void ask(intent, body);
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  };

  return (
    <Card
      title={
        <Space>
          <RobotOutlined style={{ fontSize: '20px', color: '#1890ff' }} />
          <span>Agent 问数</span>
        </Space>
      }
      style={{ height: '600px', display: 'flex', flexDirection: 'column' }}
      styles={{ body: { flex: 1, minHeight: 0, display: 'flex', flexDirection: 'column' } }}
      extra={
        messages.length > 0 ? (
          <Button
            size="small"
            type="text"
            onClick={() => {
              clear();
              onClear?.();
            }}
          >
            清空对话
          </Button>
        ) : null
      }
    >
      <Segmented
        size="small"
        block
        value={intent}
        onChange={(v) => setIntent(v as AgentIntent)}
        options={[
          { label: '生成 SQL', value: 'query' },
          { label: '诊断报错', value: 'diagnose' },
        ]}
      />
      {hint && (
        <Text type="secondary" style={{ fontSize: 12, display: 'block', margin: '6px 0 8px' }}>
          {hint}
        </Text>
      )}

      {/* 消息列表 */}
      <div style={{ flex: 1, overflowY: 'auto', margin: '8px 0 16px', paddingRight: 8 }}>
        <List
          dataSource={messages}
          locale={{ emptyText: '还没有对话。上面选一个意图，说一句试试。' }}
          renderItem={(msg) =>
            msg.role === 'system' ? (
              <List.Item
                style={{
                  justifyContent: msg.error ? 'flex-start' : 'center',
                  border: 'none',
                  padding: '2px 0',
                }}
              >
                {msg.error ? (
                  <div style={{ width: '100%', borderLeft: '3px solid #ff4d4f', paddingLeft: 10 }}>
                    <Text type="danger" style={{ fontSize: 12 }}>
                      {msg.content} · {new Date(msg.timestamp).toLocaleTimeString()}
                    </Text>
                    {/* 报错原文与当时那条语句一起留下：不写进会话，用户就得凭记忆抄一遍 */}
                    {msg.sql && <div style={RAW_BOX}>{msg.sql}</div>}
                    <div style={RAW_BOX}>{msg.error}</div>
                    <Button
                      size="small"
                      danger
                      style={{ marginTop: 6 }}
                      disabled={busy}
                      onClick={() => void ask('diagnose', msg.error as string, { sql: msg.sql })}
                    >
                      诊断这条报错
                    </Button>
                  </div>
                ) : (
                  <Text type="secondary" style={{ fontSize: 12 }}>
                    {msg.content} · {new Date(msg.timestamp).toLocaleTimeString()}
                  </Text>
                )}
              </List.Item>
            ) : (
            <List.Item
              style={{
                justifyContent: msg.role === 'user' ? 'flex-end' : 'flex-start',
              }}
            >
              <div
                style={{
                  maxWidth: '86%',
                  backgroundColor: msg.role === 'user' ? '#1890ff' : '#f0f0f0',
                  color: msg.role === 'user' ? '#fff' : '#000',
                  padding: '12px 16px',
                  borderRadius: '8px',
                  display: 'flex',
                  flexDirection: 'column',
                  alignItems: msg.role === 'user' ? 'flex-end' : 'flex-start',
                }}
              >
                <div style={{ marginBottom: 8 }}>
                  <Text strong>{msg.role === 'user' ? '你' : 'Agent'}</Text>
                </div>

                <div style={{ marginBottom: 8, whiteSpace: 'pre-wrap' }}>{msg.content}</div>

                {msg.sql && (
                  <div
                    style={{
                      backgroundColor: 'rgba(0, 0, 0, 0.05)',
                      padding: '8px',
                      borderRadius: '4px',
                      fontFamily: 'monospace',
                      fontSize: '12px',
                      maxWidth: '100%',
                      overflowX: 'auto',
                      whiteSpace: 'pre-wrap',
                    }}
                  >
                    {msg.sql}
                  </div>
                )}

                {msg.sql && onUseSql && (
                  <Button
                    size="small"
                    style={{ marginTop: 8 }}
                    onClick={() => onUseSql(msg.sql as string)}
                  >
                    填进编辑器
                  </Button>
                )}

                {/* 本机挡下的一稿：拒因点名了哪一列，可模型是单发的——重新问一遍等于
                    让它从零编。这一键把拒因和被拒的那条 SQL 一起交回去。 */}
                {msg.fix && (
                  <Tooltip title="把这条拒因和被挡下的那条 SQL 一起回喂给模型，改完仍过同一套本机校验">
                    <Button
                      size="small"
                      danger
                      style={{ marginTop: 8 }}
                      disabled={busy}
                      onClick={() => {
                        const fx = msg.fix;
                        // 固定按 query 意图重问：改稿单只可能来自 SQL 生成那条腿
                        if (fx) void ask("query", fx.question, { fix: fx });
                      }}
                    >
                      让 AI 照这条错误改
                    </Button>
                  </Tooltip>
                )}

                <Text type="secondary" style={{ fontSize: '12px', marginTop: 4 }}>
                  {new Date(msg.timestamp).toLocaleTimeString()}
                </Text>
              </div>
            </List.Item>
            )
          }
        />
        {busy && (
          <div style={{ textAlign: 'center', padding: 8 }}>
            <Spin size="small" />
            <Text type="secondary" style={{ marginLeft: 8 }}>
              正在问模型…
            </Text>
          </div>
        )}
        <div ref={messagesEndRef} />
      </div>

      {messages.length > 0 && (
        <Alert
          type="info"
          showIcon
          style={{ marginBottom: 8 }}
          title="这里不会替你执行 SQL"
          description="生成出来的语句要你自己看过再点运行；写操作另走审批门禁。"
        />
      )}

      {/* 输入框 */}
      <Space size="small" style={{ width: '100%' }}>
        <TextArea
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={INTENT_PLACEHOLDER[intent]}
          rows={2}
          style={{ flex: 1 }}
        />
        <Button type="primary" icon={<SendOutlined />} onClick={send} disabled={!input.trim() || busy} />
      </Space>
    </Card>
  );
}

export default AgentPanel;
