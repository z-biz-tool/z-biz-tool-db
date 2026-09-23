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
