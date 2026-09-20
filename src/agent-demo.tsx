// 简化的 Agent 集成示例
// 这个文件展示如何在 z-biz-tool-db 中使用 z-biz-tool-shared 的 Agent 组件

import { useState, useEffect } from "react";
import { message, Layout, Card, Space } from "antd";
import { AgentPanel } from 'z-biz-tool-shared';
import { useAgentStore } from 'z-biz-tool-shared';

const { Content } = Layout;

export default function AgentDemo() {
  const [connected, setConnected] = useState(false);
  const [dbInfo, setDbInfo] = useState({
    connectionId: '',
    databaseType: '',
    databaseName: '',
  });
  
  // Agent 状态
  const { setContext, addMessage } = useAgentStore();

  // 连接数据库时设置 Agent 上下文
  const handleConnectDatabase = async (connectionId: string, dbType: string, dbName: string) => {
    setDbInfo({
      connectionId,
      databaseType: dbType,
      databaseName: dbName,
    });
    setConnected(true);
    
    // 设置 Agent 上下文
    setContext({
      connectionId,
      databaseType: dbType,
      databaseName: dbName,
      tables: [], // TODO: 从后端获取表信息
    });
    
    message.success(`已连接到 ${dbName}`);
    
    // 添加系统消息
    addMessage({
      id: Date.now().toString(),
      role: 'system',
      content: `已连接到 ${dbName} (${dbType})`,
      timestamp: Date.now(),
    });
  };

  // 查询处理
  const handleAgentQuery = async (naturalLanguage: string) => {
    if (!connected) {
      message.warning("请先连接数据库");
      return;
    }

    try {
      const response = await useAgentStore.getState().query(naturalLanguage);
      
      if (response.success && response.sql) {
        // TODO: 执行生成的 SQL
        console.log("生成的 SQL:", response.sql);
        message.success("SQL 已生成");
      }
    } catch (error: any) {
      message.error("Agent 查询失败: " + error.message);
    }
  };

  return (
    <Layout style={{ height: '100vh' }}>
      <Content style={{ padding: 24 }}>
        <Space direction="vertical" size="large" style={{ width: '100%' }}>
          {/* 数据库连接信息 */}
          {connected && (
            <Card 
              title="数据库连接" 
              extra={
                <Space>
                  <span>类型: {dbInfo.databaseType}</span>
                  <span>名称: {dbInfo.databaseName}</span>
                </Space>
              }
            />
          )}

          {/* Agent 面板 */}
          <AgentPanel />

          {/* 示例：查询历史 */}
          {/* <QueryHistoryViewer /> */}
        </Space>
      </Content>
    </Layout>
  );
}
