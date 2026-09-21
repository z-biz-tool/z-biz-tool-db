// Agent 交互面板 - 共享组件
// 所有 z-biz-tool 项目都可以使用

import { useState, useEffect, useRef } from 'react';
import { 
  Card, 
  Input, 
  Button, 
  Space, 
  Typography, 
  Spin, 
  Avatar,
  List,
  Tag
} from 'antd';
import { RobotOutlined, SendOutlined } from '@ant-design/icons';
import { useAgentStore } from '../agent/AgentManager';

const { TextArea } = Input;
const { Text } = Typography;

export const AgentPanel: React.FC = () => {
  const [input, setInput] = useState('');
  const { messages, addMessage, query, optimize, analyze, diagnoseError } = useAgentStore();
  const [loading, setLoading] = useState(false);
  const messagesEndRef = useRef<HTMLDivElement>(null);

  const scrollToBottom = () => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  };

  useEffect(() => {
    scrollToBottom();
  }, [messages, loading]);

  const handleSend = async () => {
    if (!input.trim()) return;

    setLoading(true);
    
    try {
      // 尝试智能识别用户意图
      const text = input.toLowerCase();
      
      let response;
      if (text.includes('优化') || text.includes('改进')) {
        // 优化 SQL
        response = await optimize(input.replace(/优化|改进/i, ''));
      } else if (text.includes('分析') || text.includes('结果')) {
        // 分析结果
        response = await analyze(input.replace(/分析|结果/i, ''), {});
      } else if (text.includes('错误') || text.includes('问题')) {
        // 错误诊断
        response = await diagnoseError(input, '');
      } else {
        // 默认查询
        response = await query(input);
      }

      if (!response.success) {
        addMessage({
          id: Date.now().toString(),
          role: 'agent',
          content: `错误: ${response.error || '未知错误'}`,
          timestamp: Date.now(),
        });
      }
    } catch (error: any) {
      addMessage({
        id: Date.now().toString(),
        role: 'agent',
        content: `系统错误: ${error.message}`,
        timestamp: Date.now(),
      });
    } finally {
      setLoading(false);
      setInput('');
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  return (
    <Card
      title={
        <Space>
          <RobotOutlined style={{ fontSize: '20px', color: '#1890ff' }} />
          <span>AI Agent 助手</span>
        </Space>
      }
      style={{ height: '600px', display: 'flex', flexDirection: 'column' }}
    >
      {/* 消息列表 */}
      <div style={{ flex: 1, overflowY: 'auto', marginBottom: 16, paddingRight: 8 }}>
        <List
          dataSource={messages}
          renderItem={(msg) => (
            <List.Item
              style={{
                justifyContent: msg.role === 'user' ? 'flex-end' : 'flex-start',
              }}
            >
              <div
                style={{
                  maxWidth: '80%',
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
                  <Space size="small">
                    <Avatar
                      size={24}
                      icon={msg.role === 'user' ? <Text>A</Text> : <RobotOutlined />}
                      style={{
                        backgroundColor: msg.role === 'user' ? '#fff' : '#1890ff',
                        color: msg.role === 'user' ? '#1890ff' : '#fff',
                      }}
                    />
                    <Text strong>{msg.role === 'user' ? '你' : 'AI Agent'}</Text>
                  </Space>
                </div>
                
                <div style={{ marginBottom: 8, whiteSpace: 'pre-wrap' }}>
                  {msg.content}
                </div>

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
                    }}
                  >
                    <Text code>{msg.sql}</Text>
                  </div>
                )}

                <Text type="secondary" style={{ fontSize: '12px', marginTop: 4 }}>
                  {new Date(msg.timestamp).toLocaleTimeString()}
                </Text>
              </div>
            </List.Item>
          )}
        />
        {loading && (
          <div style={{ textAlign: 'center', padding: 8 }}>
            <Spin size="small" />
            <Text type="secondary" style={{ marginLeft: 8 }}>Agent 正在思考...</Text>
          </div>
        )}
        <div ref={messagesEndRef} />
      </div>

      {/* 输入框 */}
      <Space size="small" style={{ width: '100%' }}>
        <TextArea
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder="输入自然语言描述... (例如: 查询所有用户表)"
          rows={2}
          style={{ flex: 1 }}
        />
        <Button
          type="primary"
          icon={<SendOutlined />}
          onClick={handleSend}
          disabled={!input.trim() || loading}
        />
      </Space>

      {/* 快捷操作 */}
      <Space size="small" style={{ marginTop: 8 }} wrap>
        <Tag color="processing">查询</Tag>
        <Tag color="warning">优化</Tag>
        <Tag color="success">分析</Tag>
        <Tag color="error">诊断</Tag>
      </Space>
    </Card>
  );
};

export default AgentPanel;
