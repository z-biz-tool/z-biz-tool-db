// 内嵌浏览器视口恒为 0x0，依赖尺寸的量（recharts 画柱、CodeMirror 测量）拿不到盒子就不画。
// 探针只为验证组件逻辑，这里替一次 ResizeObserver 供固定尺寸。
export function installRoShim(width = 900, height = 320) {
  const RealRO = (window as any).ResizeObserver;
  (window as any).ResizeObserver = class {
    cb: any;
    targets: Element[] = [];
    constructor(cb: any) {
      this.cb = cb;
    }
    observe(el: Element) {
      this.targets.push(el);
      const rect = {
        width,
        height,
        top: 0,
        left: 0,
        right: width,
        bottom: height,
        x: 0,
        y: 0,
      };
      setTimeout(() => {
        this.cb([
          {
            target: el,
            contentRect: rect,
            borderBoxSize: [{ inlineSize: width, blockSize: height }],
            contentBoxSize: [{ inlineSize: width, blockSize: height }],
            devicePixelContentBoxSize: [{ inlineSize: width, blockSize: height }],
          },
        ]);
      }, 0);
    }
    unobserve(el: Element) {
      this.targets = this.targets.filter((t) => t !== el);
    }
    disconnect() {
      this.targets = [];
    }
  };
  return RealRO;
}

// 探针断言用的读文本小工具：0x0 视口点不了也截不了图，只能读 DOM
export function installReaders() {
  const w = window as any;
  w.__PROBE_TEXT = (sel: string) => document.querySelector(sel)?.textContent ?? null;
  // 最近 N 条 antd toast 的文本，用来验"提示语到底说了什么"
  // antd 6 把文字放在 .ant-message-notice-title，直接读 notice 最稳
  w.__PROBE_TOASTS = () =>
    Array.from(document.querySelectorAll(".ant-message-notice")).map(
      (n) => (n as HTMLElement).innerText?.trim() ?? ""
    );
  w.__PROBE_CLICK = (el: Element | null | undefined) => {
    if (!el) return false;
    const node = el as HTMLElement;
    node.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    node.dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
    node.click();
    return true;
  };
}
