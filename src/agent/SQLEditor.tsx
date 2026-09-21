// SQL 编辑器 - 共享组件（无外部编辑器依赖）
import { useState } from 'react';
import { Card, Button, Space, Input, message } from 'antd';
import { PlayCircleOutlined, CopyOutlined, SaveOutlined } from '@ant-design/icons';

interface SQLEditorProps {
  value?: string;
  onChange?: (value: string) => void;
  onExecute?: (sql: string) => Promise<any>;
}

export const SQLEditor: React.FC<SQLEditorProps> = ({
  value = '',
  onChange,
  onExecute,
}) => {
  const [code, setCode] = useState(value);
  const [isExecuting, setIsExecuting] = useState(false);

  const handleExecute = async () => {
    if (!code.trim()) {
      message.warning('SQL 不能为空');
      return;
    }

    setIsExecuting(true);
    try {
      await onExecute?.(code);
      message.success('查询执行成功');
    } catch (error: any) {
      message.error('查询执行失败: ' + error.message);
    } finally {
      setIsExecuting(false);
    }
  };

  return (
    <Card
      title="SQL 编辑器"
      extra={
        <Space>
          <Button
            type="primary"
            icon={<PlayCircleOutlined />}
            loading={isExecuting}
            onClick={handleExecute}
          >
            执行
          </Button>
          <Button
            icon={<CopyOutlined />}
            onClick={() => {
              navigator.clipboard.writeText(code);
              message.success('已复制到剪贴板');
            }}
          >
            复制
          </Button>
          <Button
            icon={<SaveOutlined />}
            onClick={() => {
              message.success('SQL 已保存');
            }}
          >
            保存
          </Button>
        </Space>
      }
    >
      <Input.TextArea
        value={code}
        onChange={(e) => {
          setCode(e.target.value);
          onChange?.(e.target.value);
        }}
        placeholder="输入 SQL..."
        rows={14}
        style={{
          fontFamily: 'SF Mono, Monaco, Consolas, monospace',
          fontSize: 13,
        }}
      />
    </Card>
  );
};

export default SQLEditor;
