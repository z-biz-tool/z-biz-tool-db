import { useCallback, useEffect, useState } from "react";
import {
  Alert,
  Button,
  Card,
  Col,
  Modal,
  Progress,
  Row,
  Space,
  Statistic,
  Typography,
  message,
} from "antd";
import {
  CheckCircleOutlined,
  CloudDownloadOutlined,
  ReloadOutlined,
  RocketOutlined,
} from "@ant-design/icons";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

const { Text, Paragraph } = Typography;

const formatSize = (b: number) => {
  if (!b) return "0 B";
  const k = 1024;
  const u = ["B", "KB", "MB", "GB"];
  const i = Math.min(Math.floor(Math.log(b) / Math.log(k)), u.length - 1);
  return `${(b / Math.pow(k, i)).toFixed(1)} ${u[i]}`;
};

/**
 * 自动更新闸门（2026-10-25 接入；UI 模式复用 z-biz-tool-file 的 Updater.tsx，
 * 但驱动的是官方 tauri-plugin-updater，而非手搓的 GitHub Releases 轮询）。
 *
 * - 窗口 ready 之后延时 delayMs 再 check()，不拖慢首屏；
 * - 检查失败（endpoint 404 / pubkey 仍是占位符 / 离线）一律静默降级，不打扰用户；
 * - 有更新 ⇒ 弹窗：版本号 + release notes +「立即更新 / 稍后」；
 * - 下载进度按 chunk 累加；安装完成后提示重启（一键 relaunch）。
 *
 * endpoint 与 pubkey 见 src-tauri/tauri.conf.json 的 plugins.updater；
 * pubkey 目前是 <TAURI_UPDATER_PUBKEY> 占位符，CEO 填入真实公钥前 check() 必然失败，
 * 属于预期行为（静默）。密钥与发布流程见 z-biz-tool-lead/doc/分发/updater-官方化指南.md。
 */
export default function UpdaterGate({ delayMs = 5000 }: { delayMs?: number }) {
  const [update, setUpdate] = useState<Update | null>(null);
  const [phase, setPhase] = useState<"idle" | "downloading" | "done">("idle");
  const [received, setReceived] = useState(0);
  const [total, setTotal] = useState(0);
  const [restarting, setRestarting] = useState(false);

  useEffect(() => {
    let cancelled = false;
    const timer = window.setTimeout(() => {
      void (async () => {
        try {
          const found = await check();
          if (found && !cancelled) setUpdate(found);
        } catch (e) {
          console.warn("[updater] 检查更新失败（已静默，不影响使用）:", e);
        }
      })();
    }, delayMs);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [delayMs]);

  const dismiss = useCallback(() => {
    if (phase === "downloading") return;
    // Update 在 Rust 侧持有资源，不安装就主动释放
    if (update) void update.close().catch(() => {});
    setUpdate(null);
    setPhase("idle");
    setReceived(0);
    setTotal(0);
  }, [phase, update]);

  const startUpdate = useCallback(async () => {
    if (!update) return;
    setPhase("downloading");
    setReceived(0);
    setTotal(0);
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          setTotal(event.data.contentLength ?? 0);
        } else if (event.event === "Progress") {
          setReceived((prev) => prev + event.data.chunkLength);
        }
      });
      setPhase("done");
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : String(e);
      message.error("更新下载失败: " + msg);
      setPhase("idle");
    }
  }, [update]);

  const restart = useCallback(async () => {
    setRestarting(true);
    try {
      await relaunch();
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : String(e);
      message.error("重启失败，请手动重启应用: " + msg);
      setRestarting(false);
    }
  }, []);

  if (!update) return null;

  const percent = total > 0 ? Math.min(100, Math.round((received / total) * 100)) : 0;

  return (
    <Modal
      title={
        <Space>
          <CloudDownloadOutlined style={{ color: "#1890ff" }} />
          <span>发现新版本</span>
        </Space>
      }
      open
      onCancel={dismiss}
      closable={phase !== "downloading"}
      maskClosable={phase !== "downloading"}
      width={640}
      footer={null}
      destroyOnClose
    >
      <Space direction="vertical" style={{ width: "100%" }} size={16}>
        <Row gutter={12}>
          <Col span={12}>
            <Card size="small">
              <Statistic
                title="当前版本"
                value={`v${update.currentVersion}`}
                prefix={<RocketOutlined />}
              />
            </Card>
          </Col>
          <Col span={12}>
            <Card size="small">
              <Statistic
                title="新版本"
                value={`v${update.version}`}
                valueStyle={{ color: "#52c41a" }}
                prefix={<CheckCircleOutlined />}
              />
            </Card>
          </Col>
        </Row>

        {update.body && (
          <Card size="small" title="更新内容">
            <Paragraph
              style={{
                whiteSpace: "pre-wrap",
                marginBottom: 0,
                fontSize: 12,
                maxHeight: 260,
                overflow: "auto",
              }}
            >
              {update.body}
            </Paragraph>
          </Card>
        )}

        {phase === "idle" && (
          <Space>
            <Button type="primary" icon={<CloudDownloadOutlined />} onClick={startUpdate}>
              立即更新
            </Button>
            <Button onClick={dismiss}>稍后</Button>
          </Space>
        )}

        {phase === "downloading" && (
          <Card size="small">
            <Space direction="vertical" style={{ width: "100%" }}>
              <Text>
                正在下载… {formatSize(received)}
                {total > 0 ? ` / ${formatSize(total)}` : ""}
              </Text>
              <Progress percent={percent} status="active" />
            </Space>
          </Card>
        )}

        {phase === "done" && (
          <Alert
            type="success"
            showIcon
            message="更新已下载并安装"
            description="重启应用后生效。"
            action={
              <Space>
                <Button
                  type="primary"
                  icon={<ReloadOutlined />}
                  loading={restarting}
                  onClick={restart}
                >
                  立即重启
                </Button>
                <Button onClick={dismiss}>稍后</Button>
              </Space>
            }
          />
        )}
      </Space>
    </Modal>
  );
}
