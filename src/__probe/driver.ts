// 探针驱动：0x0 视口点不了也截不了图，只能在页面里按 DOM 结构驱动一遍真实状态机。
// 只给 __probe 下的两个入口用，不进应用产物。
declare global {
  interface Window {
    __PROBE_CALLS: any[];
    [key: string]: any;
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** 勾选目录里的表（走真实 Select：展开 → 点选项） */
async function pick(labels: string[]) {
  (window as any).__PROBE_CLICK(document.querySelector(".ant-select-content"));
  await sleep(120);
  const opts = Array.from(document.querySelectorAll(".ant-select-item-option"));
  for (const l of labels) {
    const o = opts.find((x) => x.getAttribute("title") === l);
    if (!o) throw new Error(`目录里没有这张表：${l}（有 ${opts.map((x) => x.getAttribute("title")).join("|")}）`);
    (window as any).__PROBE_CLICK(o);
    await sleep(40);
  }
}

/** 填问题：受控组件必须走原生 setter，直接赋值 React 收不到 */
function setQuestion(text: string) {
  const ta = document.querySelector("textarea") as HTMLTextAreaElement;
  const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!;
  setter.call(ta, text);
  ta.dispatchEvent(new Event("input", { bubbles: true }));
}

function clickButton(text: string) {
  const b = Array.from(document.querySelectorAll("button")).find((x) =>
    (x.textContent || "").replace(/\s/g, "").includes(text.replace(/\s/g, ""))
  );
  if (!b) throw new Error(`没有这个按钮：${text}`);
  (window as any).__PROBE_CLICK(b);
}

/** 从某个下标起，某条命令收到的目录摘要 */
function sentOf(cmd: string, from: number) {
  return ((window as any).__PROBE_CALLS as any[])
    .slice(from)
    .filter((c: any) => c.cmd === cmd)
    .map((c: any) =>
      (c.args.catalog || []).map((t: any) => {
        const cols: string[] = t.columns || [];
        const typed: Record<string, string> = t.column_types || {};
        const nTypes = cols.filter((x) => (typed[x] || "").trim()).length;
        return `${t.connection_id}/${t.table}:${cols.length}/${nTypes}:${t.database_type}`;
      })
    );
}

export function installDriver() {
  const w = window as any;
  w.__probe = {
    sleep,
    pick,
    setQuestion,
    clickButton,
    marks: () => w.__PROBE_CALLS.length,
    /** 从某个下标起，某条命令的调用次数 */
    callsOf: (cmd: string, from: number) => w.__PROBE_CALLS.slice(from).filter((c: any) => c.cmd === cmd),
    /** 交给后端的目录摘要：连接/表:列数:带类型的列数:方言。
     *  列数为 0 就是 T-067 堵掉的那个洞；带类型的列数为 0 说明类型没送到模型眼前。 */
    catalogSent: (from: number) => sentOf("ai_report_draft", from),
    genCatalogSent: (from: number) => sentOf("ai_sql_generate", from),
    /** 从某个下标起，起草有没有带上上一版设计：'当时需求||数据集数/组件数'；
     *  'null' = 前端判定不该带，'undefined' = 前端根本没往 wire 上放这个参数。 */
    draftPriorSent: (from: number) =>
      (w.__PROBE_CALLS as any[])
        .slice(from)
        .filter((c: any) => c.cmd === "ai_report_draft")
        .map((c: any) => {
          const p = c.args.prior;
          if (p == null) return String(p);
          const d = p.draft || {};
          return `${p.question}||${(d.datasets || []).length}/${((d.view || {}).widgets || []).length}`;
        }),
    /** 从某个下标起，起草有没有把本机拒因回喂：'拒因长度…拒因结尾||底稿数据集数/组件数||底稿里有没有被点名的那一列'。
     *  'null' = 前端判定不该带（普通重试或从零起草），'undefined' = 连键都没上 wire。
     *  只看 feedback 分不清"喂了错误却没喂被拒稿"——模型是单发的，缺哪一半都是在凭空重画。 */
    draftFeedbackSent: (from: number) =>
      (w.__PROBE_CALLS as any[])
        .slice(from)
        .filter((c: any) => c.cmd === "ai_report_draft")
        .map((c: any) => {
          const fb = c.args.feedback;
          if (fb == null) return String(fb);
          const d = (c.args.prior || {}).draft || {};
          return `${String(fb).length}…${String(fb).slice(-18)}||${(d.datasets || []).length}/${
            ((d.view || {}).widgets || []).length
          }||${JSON.stringify(d).includes("profit_zz")}`;
        }),
    /** 从某个下标起，生成 SQL 有没有把本机拒因回喂：'拒因长度…拒因结尾||底稿SQL||底稿里有没有被点名的那一列'。
     *  'null' = 前端判定不该带（普通生成或追问），'undefined' = 连键都没上 wire。
     *  只看 feedback 分不清"喂了错误却没喂被拒的那条"——模型是单发的，缺哪一半都是在凭空重写。 */
    genFeedbackSent: (from: number) =>
      (w.__PROBE_CALLS as any[])
        .slice(from)
        .filter((c: any) => c.cmd === "ai_sql_generate")
        .map((c: any) => {
          const fb = c.args.feedback;
          if (fb == null) return String(fb);
          const base = String((c.args.prior || {}).sql || "");
          return `${String(fb).length}…${String(fb).slice(-18)}||${base}||${base.includes("net_zz")}`;
        }),
    /** AI 助手弹窗里那句自然语言需求：按 placeholder 认栏，别抓第一个 textarea
     *  （页面上 CodeMirror、规格 JSON、Agent 输入框都是 textarea）。 */
    setAiQuestion: (text: string) => {
      const ta = Array.from(
        document.querySelectorAll<HTMLTextAreaElement>(".ant-modal textarea")
      ).find((t) => (t.placeholder || "").includes("自然语言描述"));
      if (!ta) throw new Error("AI 助手弹窗里没有「自然语言描述」那一栏（先开弹窗并切到生成 SQL 页）");
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!;
      setter.call(ta, text);
      ta.dispatchEvent(new Event("input", { bubbles: true }));
    },
    /** 文案正好等于 text 的按钮：子串匹配会命中隔壁（"生成 SQL" 撞上意图「生成 SQL」、
     *  "执行" 撞上收藏弹窗），所以这里只认全等，多一个就报出来。 */
    clickExact: async (text: string) => {
      const hits = (Array.from(document.querySelectorAll("button")) as HTMLButtonElement[]).filter(
        (b) => (b.textContent || "").trim() === text
      );
      if (!hits.length) throw new Error(`没有文案正好是「${text}」的按钮`);
      if (hits.length > 1) throw new Error(`「${text}」按钮有 ${hits.length} 个，认不出点哪个`);
      if (hits[0].disabled) throw new Error(`按钮「${text}」是禁用状态`);
      w.__PROBE_CLICK(hits[0]);
      await sleep(300);
    },
    /** 挂着「照这条错误改」入口的按钮文案（报表腿与 SQL 腿共用这一句话，两处都量得动） */
    fixButtons: () =>
      Array.from(document.querySelectorAll("button"))
        .filter((b) => (b.textContent || "").includes("照这条错误改"))
        .map((b) => (b.textContent || "").trim()),
    /** 编辑器当前内容：改好的那条 SQL 有没有真落进编辑器，看这个 */
    editorText: () => (document.querySelector(".cm-content")?.textContent || "").trim(),
    /** 写规格 JSON 编辑器（手搓/写坏都走这条）。按 placeholder 认栏，别按内容认：
     *  写坏之后内容里就没有 connection_id 了。 */
    setSpec: async (text: string) => {
      const ta = document.querySelector('textarea[placeholder*="手工编辑"]') as HTMLTextAreaElement | null;
      if (!ta) throw new Error("没有规格 JSON 编辑器（先切到「规格 JSON」页签）");
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!;
      setter.call(ta, text);
      ta.dispatchEvent(new Event("input", { bubbles: true }));
      await sleep(80);
    },
    /** 从某个下标起，每次生成有没有带上上一稿：'当时需求||旧语句'，没带是 'null'，
     *  连键都没有则是 'undefined'（说明前端根本没往 wire 上放这个参数）。 */
    genPriorSent: (from: number) =>
      (w.__PROBE_CALLS as any[])
        .slice(from)
        .filter((c: any) => c.cmd === "ai_sql_generate")
        .map((c: any) =>
          c.args.prior == null ? String(c.args.prior) : `${c.args.prior.question}||${c.args.prior.sql}`
        ),
    /** 从某个下标起，取数请求里各数据集源用到的连接 id（改绑是否真落到 IPC 上看这个） */
    connsSent: (cmd: string, from: number) =>
      (w.__PROBE_CALLS as any[])
        .slice(from)
        .filter((c: any) => c.cmd === cmd)
        .map((c: any) => [
          ...new Set((c.args.datasets || []).flatMap((d: any) => (d.sources || []).map((s: any) => s.connection_id))),
        ]),
    /** 切右侧页签。别用 clickButton("报表簿")——页签文案带计数，而且左栏有
     *  一个"存入报表簿"按钮会抢先命中。 */
    clickTab: async (label: string) => {
      const flat = (s: string) => (s || "").replace(/\s+/g, "");
      const tabs = Array.from(document.querySelectorAll(".ant-tabs-tab"));
      const t = tabs.find((x) => flat(x.textContent).startsWith(flat(label)));
      if (!t) throw new Error(`没有这个页签：${label}（有 ${tabs.map((x) => (x.textContent || "").trim()).join("|")}）`);
      w.__PROBE_CLICK(t.querySelector(".ant-tabs-tab-btn") || t);
      await sleep(200);
    },
    /** 报表簿里点开某张报表的「打开并取数」 */
    openReport: async (name: string) => {
      const item = Array.from(document.querySelectorAll(".ant-list-item")).find((li) =>
        (li.textContent || "").includes(name)
      );
      if (!item) throw new Error(`报表簿里没有「${name}」（有 ${document.querySelectorAll(".ant-list-item").length} 条）`);
      const btn = Array.from(item.querySelectorAll("button")).find((b) =>
        (b.textContent || "").includes("打开并取数")
      );
      if (!btn) throw new Error(`「${name}」没有「打开并取数」按钮`);
      w.__PROBE_CLICK(btn);
      await sleep(250);
    },
    /** 改绑入口第 i 行的下拉，选到 label 命中的那条连接 */
    pickRemap: async (i: number, label: string) => {
      const card = Array.from(document.querySelectorAll(".ant-alert")).find(
        (a) => (a.textContent || "").includes("按改绑重新取数")
      );
      if (!card) throw new Error("没有改绑入口（找不到含「按改绑重新取数」的提示条）");
      const rows = card.querySelectorAll(".ant-select");
      const sel = rows[i];
      if (!sel) throw new Error(`改绑入口里没有第 ${i + 1} 个下拉（共 ${rows.length} 个）`);
      w.__PROBE_CLICK(sel.querySelector(".ant-select-content") || sel);
      await sleep(120);
      // 只看还开着的下拉面板：之前选表留下的隐藏面板会串味
      const opts = Array.from(
        document.querySelectorAll(".ant-select-dropdown:not(.ant-select-dropdown-hidden) .ant-select-item-option")
      );
      const o = opts.find((x) => (x.textContent || "").includes(label));
      if (!o) throw new Error(`下拉里没有 ${label}（有 ${opts.map((x) => x.textContent).join("|")}）`);
      w.__PROBE_CLICK(o);
      await sleep(60);
    },
    /** 起草后那张"两版差异"卡的原文（按收尾那句认，别按标题认——标题会变）。
     *  null = 这一稿没有可对照的前一版（首稿，或编辑器里本来没设计）。 */
    diffCard: () => {
      const a = Array.from(document.querySelectorAll(".ant-alert")).find((x) =>
        (x.textContent || "").includes("对照的是覆盖前后")
      );
      if (!a) return null;
      const cls = a.className;
      const kind = cls.includes("ant-alert-warning") ? "warning" : cls.includes("ant-alert-info") ? "info" : "other";
      return `${kind}|${(a.textContent || "").trim()}`;
    },
    /** 报表板上"某张集没取到数"那张卡：按标题里的说法认，返回 kind|全文 */
    failedCard: () => {
      const a = Array.from(document.querySelectorAll(".ant-alert")).find((x) =>
        (x.textContent || "").includes("没取到数")
      );
      if (!a) return null;
      const cls = a.className;
      const kind = cls.includes("ant-alert-warning")
        ? "warning"
        : cls.includes("ant-alert-error")
          ? "error"
          : "other";
      return `${kind}|${(a.textContent || "").trim().replace(/\s+/g, " ")}`;
    },
    /** "本机拒绝了这一稿"那张错误卡：返回 `whiteSpace|行数|正文（换行写成 ⏎）`。
     *  一次列全只有真按行铺开才算省了事，光数字符看不出清单有几条。 */
    rejectCard: () => {
      const a = Array.from(document.querySelectorAll(".ant-alert")).find((x) =>
        (x.textContent || "").includes("本机拒绝了这一稿")
      );
      if (!a) return null;
      const body =
        (a.querySelector(".ant-alert-description div") as HTMLElement | null) ||
        (a.querySelector(".ant-alert-description") as HTMLElement | null) ||
        a;
      const text = (body.textContent || "").trim();
      return `${getComputedStyle(body).whiteSpace}|${text.split("\n").filter(Boolean).length}|${text.replace(/\n/g, " ⏎ ")}`;
    },
    /** 画布上现在挂着哪几张图（按图卡标题认，没标题时 ChartCard 回退到组件 id） */
    chartTitles: () =>
      Array.from(document.querySelectorAll(".ant-card-head-title")).map((t) =>
        (t.textContent || "").trim().replace(/\s+/g, " ")
      ),
    /** 最近一条 toast，带成功/警告/报错色（只看文本不够：缺数据集时误报绿色成功，
     *  文案可以照抄，颜色骗不了人）。antd 把类型打在 notice 自己的 class 上。 */
    lastToast: () => {
      const n = Array.from(document.querySelectorAll(".ant-message-notice"));
      const last = n[n.length - 1] as HTMLElement | undefined;
      if (!last) return "";
      const cls = last.className;
      const kind = cls.includes("ant-message-notice-success")
        ? "success"
        : cls.includes("ant-message-notice-warning")
          ? "warning"
          : cls.includes("ant-message-notice-error")
            ? "error"
            : "other";
      return `${kind}|${(last.textContent || "").trim().replace(/\s+/g, " ")}`;
    },
    /** 当前规格 JSON 编辑器内容（按含 connection_id 的那一栏找，避开问题输入框） */
    specValue: () =>
      Array.from(document.querySelectorAll("textarea"))
        .map((t) => t.value)
        .find((v) => v.includes("connection_id")) || "",
    /** Agent 输入框：按 placeholder 认。页面里 textarea 不止一个（CodeMirror、
     *  自然语言描述、规格 JSON 都是），抓第一个会填到别处去还不报错。 */
    chatBox: () => {
      const tas = Array.from(document.querySelectorAll("textarea")) as HTMLTextAreaElement[];
      const ta = tas.find(
        (t) => (t.placeholder || "").includes("想要查什么") || (t.placeholder || "").includes("报错原文")
      );
      if (!ta) throw new Error(`没有 Agent 输入框（placeholder 有：${tas.map((x) => x.placeholder).join(" | ")}）`);
      return ta;
    },
    typeChat: (text: string) => {
      const ta = w.__probe.chatBox();
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!;
      setter.call(ta, text);
      ta.dispatchEvent(new Event("input", { bubbles: true }));
    },
    /** 意图切换：生成 SQL / 诊断报错 */
    pickIntent: async (label: string) => {
      const items = Array.from(document.querySelectorAll(".ant-segmented-item"));
      const it = items.find((x) => (x.textContent || "").trim() === label);
      if (!it) throw new Error(`没有这个意图：${label}（有 ${items.map((x) => x.textContent).join("|")}）`);
      w.__PROBE_CLICK(it.querySelector(".ant-segmented-item-label") || it);
      await sleep(80);
    },
    /** 发送：同一个 Card 里那个主按钮（只有图标，文案为空，所以按 class 认） */
    sendChat: async () => {
      const card = w.__probe.chatBox().closest(".ant-card") as Element | null;
      const btn = Array.from(card ? card.querySelectorAll("button") : []).find((b: Element) =>
        b.className.includes("ant-btn-primary")
      ) as HTMLButtonElement | undefined;
      if (!btn) throw new Error("Agent 面板里没有发送按钮");
      if (btn.disabled) throw new Error("发送按钮是禁用状态");
      w.__PROBE_CLICK(btn);
      await sleep(300);
    },
    /** 会话气泡文本（含 system 那行），用来验"用户那句有没有回声、回答有没有落屏" */
    bubbles: () =>
      Array.from(document.querySelectorAll(".ant-list-item")).map((li) =>
        (li.textContent || "").trim().replace(/\s+/g, " ")
      ),
    /** 会话里某条消息上的按钮（按文案找）。找不到就把现场按钮都吐出来，
     *  不然探针只会说"没点着"，看不出是没渲染还是文案不对。 */
    clickBubbleButton: async (label: string) => {
      const items: Element[] = Array.from(document.querySelectorAll(".ant-list-item"));
      const inList = (li: Element): HTMLButtonElement[] =>
        Array.from(li.querySelectorAll("button")) as HTMLButtonElement[];
      const all = items.flatMap(inList);
      const btn = all.find((b) => (b.textContent || "").includes(label));
      if (!btn)
        throw new Error(
          `会话里没有这个按钮：${label}（现有：${all.map((b) => (b.textContent || "").trim()).join("|") || "一个都没有"}）`
        );
      if ((btn as HTMLButtonElement).disabled) throw new Error(`按钮 ${label} 是禁用状态`);
      w.__PROBE_CLICK(btn);
      await sleep(400);
    },
    /** 顶栏"+ 新建查询"：切到新页签会把编辑器清空，用来验"报错现场用的是当时那条" */
    addQueryTab: async () => {
      const btn = document.querySelector(".ant-tabs-nav-add");
      if (!btn) throw new Error("没有新建查询的页签按钮");
      w.__PROBE_CLICK(btn);
      await sleep(150);
    },
    /** 编辑器右上角的「执行」。clickButton 会命中任何含"执行"的按钮（收藏弹窗里就有），
     *  所以这里只认文案正好等于"执行"的那一个，多一个就报出来。 */
    clickRun: async () => {
      const hits = (
        Array.from(document.querySelectorAll("button")) as HTMLButtonElement[]
      ).filter((b) => (b.textContent || "").trim() === "执行");
      if (hits.length !== 1)
        throw new Error(`"执行"按钮应当只有一个，实际 ${hits.length} 个`);
      if (hits[0].disabled) throw new Error("执行按钮是禁用状态");
      w.__PROBE_CLICK(hits[0]);
      await sleep(500);
    },
    /** 连上左侧某条连接 */
    connect: async (label: string) => {
      const li = Array.from(document.querySelectorAll(".ant-menu-item")).find((x) =>
        (x.textContent || "").includes(label)
      );
      if (!li) throw new Error(`左侧没有这条连接：${label}`);
      w.__PROBE_CLICK(li);
      await sleep(700);
    },
    /** 顶栏那个只有图标的 AI 助手按钮 → 开弹窗并切到指定页签 */
    openAiTab: async (tab: string) => {
      const btn = Array.from(document.querySelectorAll("button")).find(
        (b) => b.querySelector(".anticon-robot") && !(b.textContent || "").trim()
      );
      if (!btn) throw new Error("顶栏没有 AI 助手按钮");
      w.__PROBE_CLICK(btn);
      await sleep(600);
      w.__probe.clickTab(tab);
      await sleep(300);
    },
    /** 在「生成 SQL」页勾一张表（要先看过那一页，页签内容才在 DOM 里） */
    pickGenTable: async (label: string) => {
      const item = Array.from(document.querySelectorAll(".ant-form-item-label")).find((x) =>
        (x.textContent || "").includes("这次要用的表")
      );
      if (!item) throw new Error("没有「这次要用的表」这一项（先切到生成 SQL 页）");
      const sel = (item.closest(".ant-form-item") as Element).querySelector(".ant-select-content");
      w.__PROBE_CLICK(sel);
      await sleep(200);
      const o = Array.from(
        document.querySelectorAll(
          ".ant-select-dropdown:not(.ant-select-dropdown-hidden) .ant-select-item-option"
        )
      ).find((x) => x.getAttribute("title") === label);
      if (!o) throw new Error(`表下拉里没有 ${label}`);
      w.__PROBE_CLICK(o);
      await sleep(200);
    },
    tags: () => Array.from(document.querySelectorAll(".ant-tag")).map((t) => (t.textContent || "").trim()),
    alerts: () =>
      Array.from(document.querySelectorAll(".ant-alert")).map((a) =>
        (a.textContent || "").trim().replace(/\s+/g, " ").slice(0, 160)
      ),
    /** 起草之后的界面是否还活着：整棵树崩掉时标签和 toast 都会读空 */
    alive: () => !!document.querySelector(".ant-select-content"),
    hasDraftCard: () => document.body.textContent!.includes("草稿已过本机校验"),
  };
  w.__ERR = [];
  w.addEventListener(
    "error",
    (e: any) => w.__ERR.push(String((e.error && e.error.stack) || e.message).split("\n").slice(0, 3).join(" | ")),
    true
  );
}
