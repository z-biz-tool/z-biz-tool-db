/**
 * zbiz:// 深链接的 **db 语义层**。
 *
 * ## 分工
 *
 * | 层 | 在哪 | 管什么 |
 * |----|----|--------|
 * | 协议 | `z-biz-tool-shared/protocol/deeplink` | 字节级编解码（金标 = Rust `cap-hub`） |
 * | 机制 | `z-biz-tool-shared/protocol/deeplinkReceiver` | 冷启动补取、事件监听、不静默吞掉 |
 * | **语义** | **本文件** | **收到深链后 db 该做什么** |
 *
 * ## db 支持的动作
 *
 * | action | 含义 | 参数 |
 * |--------|------|------|
 * | `connect` | 连上某个已保存的连接 | `conn`（连接 id）或 `name`（连接名，兜底） |
 * | `open` | 不带 `conn` 时等价于 `connect` | 同上 |
 *
 * 动作取自家族规范 `z-biz-tool-lead/doc/矩阵规划/04_连接度：家族面板与统一协议.md` §2：
 * `zbiz://db/connect?conn=<id>` —— 从任意产品唤起 db 连指定库。
 *
 * ## 为什么 `conn` 与 `name` 都认
 *
 * 连接 id 是 `Date.now().toString()` 生成的（见 App.tsx 的新建连接逻辑），
 * **跨进程不稳定也不可读** —— 发深链的人（box / note / aigen）拿到的
 * 只有连接名，因为那才是用户看得见、也记得住的东西。
 *
 * 所以 id 是精确路径、name 是可读兜底。两者都给时优先 id（更精确）。
 */

import type { Handoff } from "z-biz-tool-shared/protocol";

/**
 * Rust 侧 `cap_hub::Handoff` 的镜像。
 *
 * 名字叫 `HandoffItem` 是为了在本仓内部读起来更顺（本仓此前一直用这个名字），
 * 实质就是共享层的 `Handoff`。
 */
export type { Handoff as HandoffItem } from "z-biz-tool-shared/protocol";

/**
 * db 的深链结果。
 *
 * `connectionId` / `connectionName` 非 null 表示要连某个连接；
 * `problem` 非 null 表示接不住 —— 前端必须给可见提示（不静默吞掉）。
 */
export interface DbDeepLinkOutcome {
  /** 要连的连接 id（深链直接给的）。 */
  connectionId: string | null;
  /** 要连的连接名（深链给的兜底标识）。 */
  connectionName: string | null;
  /** 发送方（from），仅用于提示与日志 */
  from: string | null;
  /** 无法适配时的原因 */
  problem: string | null;
}

/**
 * 把一条深链翻译成 db 内的动作。
 *
 * 纯函数 —— 不碰 store、不碰 Tauri，所以可以直接单测。
 *
 * @param item 深链
 */
export function resolveDbDeepLink(item: Handoff): DbDeepLinkOutcome {
  const outcome: DbDeepLinkOutcome = {
    connectionId: null,
    connectionName: null,
    from: item.params.from ?? null,
    problem: null,
  };

  const action = item.action.trim() || "open";

  if (action !== "connect" && action !== "open") {
    outcome.problem = `不认识的动作 ${action}`;
    return outcome;
  }

  const conn = item.params.conn?.trim() ?? "";
  const name = item.params.name?.trim() ?? "";

  if (conn) {
    outcome.connectionId = conn;
    // id 与 name 同时给时保留 name —— 它可能带着更友好的显示名，
    // 落地时用来在「找不到 id」的情况下给用户一句人话。
    if (name) outcome.connectionName = name;
    return outcome;
  }

  if (name) {
    outcome.connectionName = name;
    return outcome;
  }

  outcome.problem = "深链里没有 conn（连接 id）或 name（连接名）参数";
  return outcome;
}
