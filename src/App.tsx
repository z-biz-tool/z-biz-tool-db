import { useState } from "react";
import { ConfigProvider, theme, Layout, Menu, Button, Space, Tabs, Card, Table, Input, Select, Form, Modal, message, Tooltip } from "antd";
import {
  DatabaseOutlined,
  PlusOutlined,
  PlayCircleOutlined,
  FormatPainterOutlined,
  CopyOutlined,
  DeleteOutlined,
  EditOutlined,
  SaveOutlined,
  SyncOutlined,
  TableOutlined,
  CodeOutlined,
  HistoryOutlined,
  SettingOutlined,
  CloudOutlined,
  ThunderboltOutlined,
} from "@ant-design/icons";

const { Header, Sider, Content } = Layout;
const { TextArea } = Input;
const { TabPane } = Tabs;

// 数据库连接类型
interface DBConnection {
  id: string;
  name: string;
  type: "mysql" | "postgresql" | "sqlite" | "sqlserver";
  host: string;
  port: number;
  username: string;
  password: string;
  database: string;
}

// 模拟数据
const mockConnections: DBConnection[] = [
  {
    id: "1",
    name: "本地 MySQL",
    type: "mysql",
    host: "localhost",
    port: 3306,
    username: "root",
    password: "",
    database: "test_db",
  },
  {
    id: "2",
    name: "生产 PostgreSQL",
    type: "postgresql",
    host: "192.168.1.100",
    port: 5432,
    username: "admin",
    password: "",
    database: "production",
  },
];

function App() {
  const [darkMode, setDarkMode] = useState(false);
  const [connections, setConnections] = useState<DBConnection[]>(mockConnections);
  const [selectedConnection, setSelectedConnection] = useState<DBConnection | null>(null);
  const [sqlCode, setSqlCode] = useState("SELECT * FROM users WHERE id = 1");
  const [queryResults, setQueryResults] = useState<any[]>([]);
  const [isConnected, setIsConnected] = useState(false);
  const [showConnectionModal, setShowConnectionModal] = useState(false);
  const [editingConnection, setEditingConnection] = useState<DBConnection | null>(null);
  const [form] = Form.useForm();

  // 执行 SQL 查询
  const executeQuery = () => {
    // 模拟查询结果
    setQueryResults([
      { id: 1, name: "张三", email: "zhangsan@example.com", age: 25 },
      { id: 2, name: "李四", email: "lisi@example.com", age: 30 },
      { id: 3, name: "王五", email: "wangwu@example.com", age: 28 },
    ]);
    message.success("查询执行成功");
  };

  // 格式化 SQL
  const formatSQL = () => {
    // 简单的格式化示例
    const formatted = sqlCode
      .replace(/\bSELECT\b/g, "SELECT\n  ")
      .replace(/\bFROM\b/g, "\nFROM\n  ")
      .replace(/\bWHERE\b/g, "\nWHERE\n  ")
      .replace(/\bAND\b/g, "\n  AND")
      .replace(/\bOR\b/g, "\n  OR")
      .replace(/\bORDER BY\b/g, "\nORDER BY\n  ")
      .replace(/\bGROUP BY\b/g, "\nGROUP BY\n  ")
      .replace(/\bHAVING\b/g, "\nHAVING\n  ")
      .replace(/\bLIMIT\b/g, "\nLIMIT")
      .replace(/\bJOIN\b/g, "\nJOIN")
      .replace(/\bLEFT JOIN\b/g, "\nLEFT JOIN")
      .replace(/\bRIGHT JOIN\b/g, "\nRIGHT JOIN")
      .replace(/\bINNER JOIN\b/g, "\nINNER JOIN");
    setSqlCode(formatted);
    message.success("SQL 已格式化");
  };

  // 复制 SQL
  const copySQL = () => {
    navigator.clipboard.writeText(sqlCode);
    message.success("SQL 已复制到剪贴板");
  };

  // 连接数据库
  const connectDB = (connection: DBConnection) => {
    setSelectedConnection(connection);
    setIsConnected(true);
    message.success(`已连接到 ${connection.name}`);
  };

  // 断开连接
  const disconnectDB = () => {
    setIsConnected(false);
    setSelectedConnection(null);
    setQueryResults([]);
    message.info("已断开连接");
  };

  // 保存连接
  const saveConnection = (values: any) => {
    if (editingConnection) {
      // 更新现有连接
      setConnections(
        connections.map((c) =>
          c.id === editingConnection.id ? { ...c, ...values } : c
        )
      );
      message.success("连接已更新");
    } else {
      // 创建新连接
      const newConnection: DBConnection = {
        id: Date.now().toString(),
        ...values,
      };
      setConnections([...connections, newConnection]);
      message.success("连接已创建");
    }
    setShowConnectionModal(false);
    setEditingConnection(null);
    form.resetFields();
  };

  // 删除连接
  const deleteConnection = (id: string) => {
    Modal.confirm({
      title: "确认删除",
      content: "确定要删除这个连接吗？",
      onOk: () => {
        setConnections(connections.filter((c) => c.id !== id));
        message.success("连接已删除");
      },
    });
  };

  // 编辑连接
  const editConnection = (connection: DBConnection) => {
    setEditingConnection(connection);
    form.setFieldsValue(connection);
    setShowConnectionModal(true);
  };

  // 表格列定义
  const resultColumns = queryResults.length > 0
    ? Object.keys(queryResults[0]).map((key) => ({
        title: key,
        dataIndex: key,
        key: key,
        ellipsis: true,
      }))
    : [];

  return (
    <ConfigProvider
      theme={{
        algorithm: darkMode ? theme.darkAlgorithm : theme.defaultAlgorithm,
      }}
    >
      <Layout style={{ height: "100vh" }}>
        <Sider width={280} theme={darkMode ? "dark" : "light"} style={{ borderRight: "1px solid #f0f0f0" }}>
          <div style={{ padding: "16px", borderBottom: "1px solid #f0f0f0" }}>
            <Space>
              <DatabaseOutlined style={{ fontSize: "20px" }} />
              <strong>数据库连接</strong>
            </Space>
          </div>
          <div style={{ padding: "8px" }}>
            <Button
              type="primary"
              icon={<PlusOutlined />}
              block
              onClick={() => {
                setEditingConnection(null);
                form.resetFields();
                setShowConnectionModal(true);
              }}
            >
              新建连接
            </Button>
          </div>
          <Menu
            mode="inline"
            theme={darkMode ? "dark" : "light"}
            style={{ borderRight: 0 }}
            items={connections.map((conn) => ({
              key: conn.id,
              icon: <DatabaseOutlined />,
              label: (
                <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                  <span>{conn.name}</span>
                  <Space size="small">
                    <Tooltip title="编辑">
                      <EditOutlined
                        onClick={(e) => {
                          e.stopPropagation();
                          editConnection(conn);
                        }}
                        style={{ color: "#1890ff" }}
                      />
                    </Tooltip>
                    <Tooltip title="删除">
                      <DeleteOutlined
                        onClick={(e) => {
                          e.stopPropagation();
                          deleteConnection(conn.id);
                        }}
                        style={{ color: "#ff4d4f" }}
                      />
                    </Tooltip>
                  </Space>
                </div>
              ),
              onClick: () => connectDB(conn),
            }))}
          />
        </Sider>
        <Layout>
          <Header
            style={{
              background: darkMode ? "#141414" : "#fff",
              padding: "0 24px",
              borderBottom: "1px solid #f0f0f0",
              display: "flex",
              justifyContent: "space-between",
              alignItems: "center",
            }}
          >
            <Space>
              {isConnected && selectedConnection && (
                <>
                  <Tag color="green">已连接</Tag>
                  <span>{selectedConnection.name}</span>
                  <Button size="small" onClick={disconnectDB}>
                    断开连接
                  </Button>
                </>
              )}
            </Space>
            <Space>
              <Tooltip title="切换主题">
                <Button
                  icon={darkMode ? <ThunderboltOutlined /> : <CloudOutlined />}
                  onClick={() => setDarkMode(!darkMode)}
                />
              </Tooltip>
              <Tooltip title="设置">
                <Button icon={<SettingOutlined />} />
              </Tooltip>
            </Space>
          </Header>
          <Content style={{ display: "flex", flexDirection: "column" }}>
            {isConnected ? (
              <>
                {/* SQL 编辑器区域 */}
                <Card
                  size="small"
                  style={{ margin: "8px", flex: "0 0 auto" }}
                  title="SQL 编辑器"
                  extra={
                    <Space>
                      <Button icon={<FormatPainterOutlined />} onClick={formatSQL}>
                        格式化
                      </Button>
                      <Button icon={<CopyOutlined />} onClick={copySQL}>
                        复制
                      </Button>
                      <Button type="primary" icon={<PlayCircleOutlined />} onClick={executeQuery}>
                        执行
                      </Button>
                    </Space>
                  }
                >
                  <TextArea
                    value={sqlCode}
                    onChange={(e) => setSqlCode(e.target.value)}
                    autoSize={{ minRows: 3, maxRows: 10 }}
                    style={{ fontFamily: "monospace", fontSize: "14px" }}
                    placeholder="输入 SQL 语句..."
                  />
                </Card>

                {/* 查询结果区域 */}
                <Card
                  size="small"
                  style={{ margin: "8px", flex: 1, overflow: "hidden" }}
                  title={`查询结果 (${queryResults.length} 行)`}
                  extra={
                    <Space>
                      <Button icon={<SaveOutlined />}>导出</Button>
                      <Button icon={<SyncOutlined />}>刷新</Button>
                    </Space>
                  }
                >
                  <Table
                    columns={resultColumns}
                    dataSource={queryResults.map((row, index) => ({ ...row, key: index }))}
                    size="small"
                    scroll={{ x: "max-content", y: 300 }}
                    pagination={{ pageSize: 100 }}
                  />
                </Card>
              </>
            ) : (
              <div
                style={{
                  display: "flex",
                  flexDirection: "column",
                  justifyContent: "center",
                  alignItems: "center",
                  height: "100%",
                  color: "#999",
                }}
              >
                <DatabaseOutlined style={{ fontSize: "64px", marginBottom: "16px" }} />
                <h2>数据库管理工具</h2>
                <p>请从左侧选择一个连接或创建新连接</p>
              </div>
            )}
          </Content>
        </Layout>
      </Layout>

      {/* 连接编辑弹窗 */}
      <Modal
        title={editingConnection ? "编辑连接" : "新建连接"}
        open={showConnectionModal}
        onOk={() => form.submit()}
        onCancel={() => {
          setShowConnectionModal(false);
          setEditingConnection(null);
          form.resetFields();
        }}
      >
        <Form form={form} layout="vertical" onFinish={saveConnection}>
          <Form.Item name="name" label="连接名称" rules={[{ required: true, message: "请输入连接名称" }]}>
            <Input placeholder="例如：本地 MySQL" />
          </Form.Item>
          <Form.Item name="type" label="数据库类型" rules={[{ required: true, message: "请选择数据库类型" }]}>
            <Select
              placeholder="选择数据库类型"
              options={[
                { value: "mysql", label: "MySQL" },
                { value: "postgresql", label: "PostgreSQL" },
                { value: "sqlite", label: "SQLite" },
                { value: "sqlserver", label: "SQL Server" },
              ]}
            />
          </Form.Item>
          <Form.Item name="host" label="主机地址" rules={[{ required: true, message: "请输入主机地址" }]}>
            <Input placeholder="例如：localhost" />
          </Form.Item>
          <Form.Item name="port" label="端口" rules={[{ required: true, message: "请输入端口" }]}>
            <Input type="number" placeholder="例如：3306" />
          </Form.Item>
          <Form.Item name="username" label="用户名" rules={[{ required: true, message: "请输入用户名" }]}>
            <Input placeholder="例如：root" />
          </Form.Item>
          <Form.Item name="password" label="密码">
            <Input.Password placeholder="输入密码" />
          </Form.Item>
          <Form.Item name="database" label="数据库名" rules={[{ required: true, message: "请输入数据库名" }]}>
            <Input placeholder="例如：my_database" />
          </Form.Item>
        </Form>
      </Modal>
    </ConfigProvider>
  );
}

export default App;