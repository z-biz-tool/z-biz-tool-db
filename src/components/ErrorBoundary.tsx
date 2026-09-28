import React from "react";
import { Alert, Button, Space } from "antd";

interface ErrorBoundaryProps {
  /** 出错区域的名字，进提示文案（"报表看板" 而不是"某处"） */
  label: string;
  children: React.ReactNode;
  /**
   * 变了就自动重置边界。用来兜住"换一份数据又炸一次"的场景：
   * 用户点了重试但引发崩溃的输入还在，不重置就会一直停在错误页。
   */
  resetKey?: unknown;
  /** 根边界：崩了就整页不可用，不能说"其余功能仍可正常使用" */
  fatal?: boolean;
}

interface ErrorBoundaryState {
  error: Error | null;
}

/**
 * 子树崩溃时保住整个应用。
 *
 * 这个应用以前没有任何边界：报表/Agent 子树里一次抛错会把整棵 React 树连带
 * 正在编辑的 SQL 一起白屏，用户唯一的出路是重启应用。
 */
export class ErrorBoundary extends React.Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    console.error(`[${this.props.label}] 渲染崩溃:`, error, info.componentStack);
  }

  componentDidUpdate(prev: ErrorBoundaryProps) {
    if (this.state.error && prev.resetKey !== this.props.resetKey) {
      this.setState({ error: null });
    }
  }

  private reset = () => this.setState({ error: null });

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    return (
      <Alert
        type="error"
        showIcon
        style={{ margin: 16 }}
        message={
          this.props.fatal
            ? `${this.props.label} 崩溃，界面已停在这一步`
            : `${this.props.label} 出了问题，其余功能仍可正常使用`
        }
        description={
          <Space direction="vertical" style={{ width: "100%" }}>
            <span>{error.message || "未知错误"}</span>
            <Space>
              <Button size="small" onClick={this.reset}>
                重试这一块
              </Button>
              <Button
                size="small"
                onClick={() =>
                  void navigator.clipboard.writeText(
                    `${error.message}\n${error.stack ?? ""}`,
                  )
                }
              >
                复制错误详情
              </Button>
            </Space>
          </Space>
        }
      />
    );
  }
}

export default ErrorBoundary;
