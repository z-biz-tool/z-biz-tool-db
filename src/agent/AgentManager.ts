import { create } from "zustand";
import { AgentIntent, AgentMessage, AgentResponse } from "./types";

/** 宿主应用登记的真实实现：面板只收意图和那句话，不认识任何后端命令。 */
export type AgentHandler = (intent: AgentIntent, text: string) => Promise<AgentResponse>;

interface AgentStore {
  messages: AgentMessage[];
  /** 这一轮还在等后端 */
  busy: boolean;
  handler: AgentHandler | null;
  addMessage: (message: AgentMessage) => void;
  setHandler: (handler: AgentHandler | null) => void;
  clear: () => void;
  ask: (intent: AgentIntent, text: string) => Promise<void>;
}

/** 面板可以被没有后端的宿主单独挂上去，那时不许伪造回答（T-005 那条闸）。 */
const NO_HANDLER = "没有可用的后端实现：宿主应用没给 Agent 登记处理函数";

const nextId = () => `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;

export const useAgentStore = create<AgentStore>((set, get) => ({
  messages: [],
  busy: false,
  handler: null,

  addMessage: (message) =>
    set((state) => ({ messages: [...state.messages, message] })),

  setHandler: (handler) => set({ handler }),

  clear: () => set({ messages: [] }),

  /** 一轮对话：先把用户那句贴进会话，再交给宿主登记的实现。
   *  两边的消息都在 store 里落账，面板才不会"发出去却什么都没看见"。 */
  ask: async (intent, text) => {
    const body = text.trim();
    if (!body || get().busy) return;
    set((state) => ({
      busy: true,
      messages: [
        ...state.messages,
        { id: nextId(), role: "user", content: body, timestamp: Date.now() },
      ],
    }));
    const reply = (content: string, sql?: string) =>
      set((state) => ({
        messages: [
          ...state.messages,
          { id: nextId(), role: "agent", content, sql, timestamp: Date.now() },
        ],
      }));
    const handler = get().handler;
    try {
      if (!handler) {
        reply(`错误: ${NO_HANDLER}`);
        return;
      }
      const res = await handler(intent, body);
      if (res.success) reply(res.content, res.sql);
      else reply(`错误: ${res.error || "未知错误"}`);
    } catch (e: any) {
      reply(`系统错误: ${e?.message || e}`);
    } finally {
      set({ busy: false });
    }
  },
}));
