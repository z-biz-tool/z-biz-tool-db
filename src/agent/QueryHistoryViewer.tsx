// 查询历史查看器 - 共享组件
import { useState, useEffect } from 'react';
import { Table, Tag, Card, Button, Typography } from 'antd';
import type { TableColumnsType } from 'antd';
import { CheckCircleOutlined, CloseCircleOutlined, CopyOutlined } from '@ant-design/icons';

export interface QueryHistoryItem {
  id: string;
  naturalLanguage: string;
  generatedSql: string;
  feedback: number;
  executionTimeMs?: number;
  success: boolean;
  createdAt: string;
}

interface QueryHistoryViewerProps {
  onCopySql?: (sql: string) => void;
}

export const QueryHistoryViewer: React.FC<QueryHistoryViewerProps> = ({ onCopySql }) => {
  const [history] = useState<QueryHistoryItem[]>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    loadHistory();
  }, []);

  const loadHistory = async () => {
    setLoading(true);
    try {
      // TODO: 调用后端 API
      // const result = await invoke("load_history");
      // setHistory(result);
    } finally {
      setLoading(false);
    }
  };

  const columns: TableColumnsType<QueryHistoryItem> = [
    {
      title: '自然语言描述',
      dataIndex: 'naturalLanguage',
      key: 'naturalLanguage',
      width: 200,
      ellipsis: true,
    },
    {
      title: '生成的 SQL',
      dataIndex: 'generatedSql',
      key: 'generatedSql',
      width: 300,
      ellipsis: true,
      render: (sql) => (
        <Typography.Text code style={{ maxWidth: 300, display: 'block' }}>
          {sql}
        </Typography.Text>
      ),
    },
    {
      title: '执行状态',
      dataIndex: 'success',
      key: 'success',
      width: 100,
      render: (success: boolean) => (
        <Tag icon={success ? <CheckCircleOutlined /> : <CloseCircleOutlined />}>
          {success ? '成功' : '失败'}
        </Tag>
      ),
    },
    {
      title: '用户反馈',
      dataIndex: 'feedback',
      key: 'feedback',
      width: 100,
      render: (feedback: number) => {
        if (feedback === 1) return <Tag color="success">👍 好</Tag>;
        if (feedback === 0) return <Tag color="default">😐 一般</Tag>;
        if (feedback === -1) return <Tag color="error">👎 差</Tag>;
        return <Tag>无</Tag>;
      },
    },
    {
      title: '耗时',
      dataIndex: 'executionTimeMs',
      key: 'executionTimeMs',
      width: 100,
      render: (ms?: number) => (ms ? `${ms}ms` : '-'),
    },
    {
      title: '操作',
      key: 'actions',
      width: 100,
      render: (_, record) => (
        <Button
          size="small"
          icon={<CopyOutlined />}
          onClick={() => onCopySql?.(record.generatedSql)}
        >
          复制
        </Button>
      ),
    },
  ];

  return (
    <Card
      title="查询历史"
      extra={<Button size="small" loading={loading} onClick={loadHistory}>刷新</Button>}
      style={{ marginTop: 16 }}
    >
      <Table
        columns={columns}
        dataSource={history}
        rowKey="id"
        loading={loading}
        pagination={{ pageSize: 10 }}
      />
    </Card>
  );
};

export default QueryHistoryViewer;
