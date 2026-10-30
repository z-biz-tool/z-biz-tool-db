/**
 * IPC 契约（后端 invoke_handler ↔ 前端 invoke 调用点）。
 *
 * 跑法：node --experimental-strip-types --test tests/ipcContract.test.ts
 *
 * 为什么要这道门：**「后端实现了、注册了，但界面上没有入口」是查不出来的**。
 * 2026-10-22 实测：db 仓 47 个已注册命令里，有 16 个前端一次都没调过 ——
 * 其中 `save_query` / `delete_saved_query` 对应的「常用查询」在界面上**只写进 React state**，
 * 却弹「已保存到常用查询」，刷新页面就没了 ⇒ 典型的「界面说成功、事实不是」。
 *
 * 这道门守五件事：
 * 1) 前端 invoke 的命令，后端必须真的注册了（否则点了才炸）；
 * 2) 前端传的参数名，后端签名里必须有（拼错 ⇒ 静默收到 None ⇒ 功能悄悄降级）；
 * 3) 后端的必填参数，前端都传了（漏传 ⇒ 运行时才发现）；
 * 4) 后端注册的命令，**要么被前端调用，要么在「预留未接线」清单里显式声明并写明理由**
 *    （不许有既没入口、也没登记的功能）；
 * 5) 「预留未接线」清单里的命令，必须真的没有被前端调用（接了就该从清单里删掉）。
 *
 * 口径说明：
 * - 扫描范围：前端只扫 `src/`（不含 tests/）；后端扫 `src-tauri/src/`。
 * - Tauri v2 的参数命名：Rust 的 snake_case 参数在 JS 侧用 camelCase。
 * - 注释里的 invoke 调用不算调用点（先剥注释再扫）。
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const here = fileURLToPath(new URL(".", import.meta.url));
const repoRoot = path.resolve(here, "..");

/* ────────────────────────── 后端 ────────────────────────── */

interface RustParam {
  /** Rust 侧原名（snake_case） */
  snake: string;
  /** JS 侧要用的名字（camelCase） */
  camel: string;
  optional: boolean;
  /** Tauri 注入的参数（前端不能传） */
  injected: boolean;
  /** 完整类型文本（用来认嵌套结构体） */
  ty: string;
}

interface RustCommand {
  name: string;
  params: RustParam[];
  file: string;
}

/** 后端可反序列化的结构体：名字 → 字段 */
interface RustStruct {
  /** 字段名（serde 原样，snake_case） */
  fields: Set<string>;
  /** 缺省就不算「漏传」的字段（Option / #[serde(default)]） */
  defaulted: Set<string>;
}

function walk(dir: string, exts: string[]): string[] {
  const out: string[] = [];
  for (const e of readdirSync(dir)) {
    const p = path.join(dir, e);
    const st = statSync(p);
    if (st.isDirectory()) {
      if (e === "node_modules" || e === "target" || e === "dist") continue;
      out.push(...walk(p, exts));
    } else if (exts.some((x) => e.endsWith(x))) {
      out.push(p);
    }
  }
  return out;
}

function toCamel(s: string): string {
  return s.replace(/_([a-z0-9])/g, (_, c: string) => c.toUpperCase());
}

/** 按顶层逗号切分（`Option<Vec<String>>`、`{ a: 1, b: 2 }` 里的逗号都不算） */
function splitTopLevel(s: string): string[] {
  const out: string[] = [];
  let depth = 0;
  let cur = "";
  for (const ch of s) {
    if (ch === "<" || ch === "(" || ch === "[" || ch === "{") depth++;
    else if (ch === ">" || ch === ")" || ch === "]" || ch === "}") depth--;
    if (ch === "," && depth === 0) {
      out.push(cur);
      cur = "";
    } else {
      cur += ch;
    }
  }
  if (cur.trim()) out.push(cur);
  return out.map((x) => x.trim()).filter(Boolean);
}

/** 扫出所有带 #[command] / #[tauri::command] 的 Rust 函数 */
function backendCommands(): Map<string, RustCommand> {
  const out = new Map<string, RustCommand>();
  const files = walk(path.join(repoRoot, "src-tauri/src"), [".rs"]);
  const re =
    /#\[(?:tauri::)?command(?:\([^\]]*\))?\]\s*(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(([^)]*)\)/g;
  for (const f of files) {
    const src = readFileSync(f, "utf8");
    let m: RegExpExecArray | null;
    while ((m = re.exec(src)) !== null) {
      const name = m[1];
      const params: RustParam[] = [];
      for (const raw of splitTopLevel(m[2])) {
        const pm = raw.match(/^([A-Za-z_][A-Za-z0-9_]*)\s*:\s*(.+)$/s);
        if (!pm) continue;
        const snake = pm[1];
        const ty = pm[2].trim();
        const injected = /\b(State|AppHandle|Window|WebviewWindow)\b/.test(ty);
        params.push({
          snake,
          camel: toCamel(snake),
          optional: ty.startsWith("Option<") || injected,
          injected,
          ty,
        });
      }
      out.set(name, { name, params, file: path.relative(repoRoot, f) });
    }
  }
  return out;
}

/** invoke_handler![] 里真正注册的命令名（取叶子名） */
function registeredCommands(): Set<string> {
  const lib = readFileSync(path.join(repoRoot, "src-tauri/src/lib.rs"), "utf8");
  const out = new Set<string>();
  const re = /generate_handler!\s*\[/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(lib)) !== null) {
    let i = m.index + m[0].length - 1;
    let depth = 0;
    for (; i < lib.length; i++) {
      if (lib[i] === "[") depth++;
      else if (lib[i] === "]") {
        depth--;
        if (depth === 0) break;
      }
    }
    const body = lib.slice(m.index + m[0].length, i);
    for (const cm of body.matchAll(/([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)\s*(?:::|,|$)/gm)) {
      out.add(cm[1].split("::").pop()!);
    }
  }
  return out;
}

/**
 * 扫出后端**可反序列化的结构体**（供嵌套参数校验用）。
 *
 * 为什么需要它：Tauri v2 的**顶层**命令参数在 JS 侧是 camelCase，
 * 但**嵌套结构体字段走 serde**（本仓没有 `rename_all`）⇒ 仍是 snake_case。
 * 两套规则不一样 ⇒ 嵌套字段拼错时 serde **静默忽略**（不报错），
 * 于是「审批参数没送到」表现为「后端说只读模式拒绝导入」——
 * 静默降级，且用户完全看不出来是自己点错了还是程序坏了。
 *
 * 所以这道门必须单独覆盖嵌套字段。
 */
function backendStructs(): Map<string, RustStruct> {
  const out = new Map<string, RustStruct>();
  const files = walk(path.join(repoRoot, "src-tauri/src"), [".rs"]);
  for (const f of files) {
    // 先剥注释：字段的文档注释里也会出现 `pub xxx:` 形状的字样
    const src = stripComments(readFileSync(f, "utf8"));
    const re = /#\[derive\(([^)]*)\)\]\s*pub struct ([A-Za-z_][A-Za-z0-9_]*)\s*\{/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(src)) !== null) {
      const derives = m[1];
      if (!/Deserialize/.test(derives)) continue;
      const name = m[2];
      const body = src.slice(m.index + m[0].length);
      const end = body.indexOf("\n}");
      if (end < 0) continue;
      const fields = new Set<string>();
      const defaulted = new Set<string>();
      for (const line of body.slice(0, end).split("\n")) {
        const fm = line.match(/^\s*pub\s+([A-Za-z_][A-Za-z0-9_]*)\s*:\s*(.+?),?\s*$/);
        if (!fm) continue;
        fields.add(fm[1]);
        if (/^Option</.test(fm[2].trim()) || /#\[serde\(default/.test(line)) {
          defaulted.add(fm[1]);
        }
      }
      out.set(name, { fields, defaulted });
    }
  }
  return out;
}

/** 从类型文本里取出结构体名：`Option<ImportGate>` → `ImportGate` */
function structNameOf(ty: string): string | null {
  const m = ty.trim().match(/^(?:Option<)?([A-Z][A-Za-z0-9_]*)/);
  return m ? m[1] : null;
}

/* ────────────────────────── 前端 ────────────────────────── */

interface FrontCall {
  cmd: string;
  file: string;
  line: number;
  keys: string[];
  /** 顶层 key → 该 key 的对象字面量里的键（嵌套结构体参数） */
  nested: Map<string, string[]>;
}

/** 剥掉注释，避免注释里的 invoke 被当成调用点 */
function stripComments(src: string): string {
  return src.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
}

/**
 * 读出对象字面量的顶层键
 */
function objectKeys(obj: string): string[] {
  const keys: string[] = [];
  for (const seg of splitTopLevel(obj)) {
    const km = seg.match(/^(?:"([^"]+)"|'([^']+)'|([A-Za-z_$][\w$]*))\s*(?::|$)/);
    if (km) keys.push(km[1] ?? km[2] ?? km[3]);
  }
  return keys;
}

/**
 * 读出「顶层 key → 值是对象字面量的那些 key 的内层键」。
 *
 * 嵌套结构体参数（serde 语义）就靠它来校验。
 */
function nestedObjectKeys(obj: string): Map<string, string[]> {
  const out = new Map<string, string[]>();
  for (const seg of splitTopLevel(obj)) {
    const km = seg.match(/^(?:"([^"]+)"|'([^']+)'|([A-Za-z_$][\w$]*))\s*:\s*\{/);
    if (!km) continue;
    const key = km[1] ?? km[2] ?? km[3];
    // 从这个 `{` 开始找配对的 `}`
    const start = seg.indexOf("{", seg.indexOf(":"));
    let depth = 0;
    let end = -1;
    for (let i = start; i < seg.length; i++) {
      if (seg[i] === "{") depth++;
      else if (seg[i] === "}") {
        depth--;
        if (depth === 0) {
          end = i;
          break;
        }
      }
    }
    if (end < 0) continue;
    out.set(key, objectKeys(seg.slice(start + 1, end)));
  }
  return out;
}

function frontendInvokes(): FrontCall[] {
  const out: FrontCall[] = [];
  const files = walk(path.join(repoRoot, "src"), [".ts", ".tsx"]);
  const re = /\binvoke(?:<[^>]*>)?\s*\(/g;
  for (const f of files) {
    const raw = readFileSync(f, "utf8");
    const src = stripComments(raw);
    // 行号要按剥注释前的原文算，所以逐行对应
    const rawLines = raw.split("\n");
    let m: RegExpExecArray | null;
    while ((m = re.exec(src)) !== null) {
      const i = m.index + m[0].length - 1;
      // 命令名：紧跟的字符串字面量
      const sm = src.slice(i + 1).match(/^\s*(?:"([^"]+)"|'([^']+)'|`([^`]+)`)/);
      if (!sm) continue;
      const cmd = sm[1] ?? sm[2] ?? sm[3];
      // 参数：逗号后若为对象字面量，取其顶层键
      let keys: string[] = [];
      let nested = new Map<string, string[]>();
      const after = src.slice(i + 1 + sm[0].length);
      const om = after.match(/^\s*,\s*\{/);
      if (om) {
        const objStart = i + 1 + sm[0].length + om[0].length - 1;
        let depth = 0;
        let j = objStart;
        for (; j < src.length; j++) {
          if (src[j] === "{") depth++;
          else if (src[j] === "}") {
            depth--;
            if (depth === 0) break;
          }
        }
        const body = src.slice(objStart + 1, j);
        keys = objectKeys(body);
        nested = nestedObjectKeys(body);
      }
      const line = rawLines.slice(0, src.slice(0, m.index).split("\n").length - 1).length;
      out.push({ cmd, file: path.relative(repoRoot, f), line, keys, nested });
    }
  }
  return out;
}

/* ────────────────────────── 预留未接线清单 ────────────────────────── */

/**
 * 刻意不做界面入口的命令，每条都要写明理由。
 * 清单不是「豁免名单」—— 门禁会反过来检查它：接了就必须删掉。
 */
const RESERVED_NOT_WIRED = new Map<string, string>([
  [
    "check_connection_revision",
    "并发修订一致性护栏。前端调用它并不能防住竞态（检查与实际使用之间仍有时间窗），" +
      "由后端在执行路径内部自校验才有效 ⇒ 前端入口是假的防线。",
  ],
  [
    "metadata_list_v2",
    "带 cache_version 的统一对象协议（v2 包络）。前端已有 get_tables / get_table_structure " +
      "两个可用入口，再挂第三套只会让界面多一条口径 ⇒ 等协议定稿再一次性切。",
  ],
  [
    "keyset_query",
    "游标分页需要列表页上下文（当前界面没有分页列表视图）⇒ 这轮没有可挂的界面位置，" +
      "等有分页视图再接，避免为它先造一个半成品入口。",
  ],
  [
    "export_connections",
    "后端这份会把密码写进导出文件。前端已有一份**不含密码**的导出实现（更安全）" +
      "⇒ 刻意不接，避免给用户一个会把明文密码落到磁盘上的入口。",
  ],
  [
    "run_secret_migration",
    "凭证类动作，默认不做。密钥迁移是一次性运维动作，应由运维显式触发，" +
      "不该藏在日常工具的菜单里；且 load_connections 已在取不到密钥时显式上报" +
      "「需重新输入密码」，不依赖这个命令也能安全降级。",
  ],
  [
    "zbiz_take_pending",
    "zbiz:// 深链队列的前端消费口。协议与队列在 capability 仓的 cap-hub 里，" +
      "本仓 Rust 侧已接（box 用 zbiz://db/open?query=… 唤起本仓时靠它入队），" +
      "但本仓界面的语义层（拿到 query 后干什么）尚未设计 —— aigen 是本轮试点，" +
      "其余仓铺开时一并接界面并从本清单删除。",
  ],
]);

/* ────────────────────────── 有意省略的可选参数 ────────────────────────── */

/**
 * 后端有、前端**一次都没传**的可选参数。
 *
 * 为什么单列一张清单：可选参数漏传**不报错**，后端按「调用方没要求」走 ⇒
 * 功能静默降级（例如 CAS 护栏不生效、行数上限失效）。这类降级没人会发现，
 * 所以必须逐条显式声明：要么前端传，要么写明为什么不传。
 */
const MAYBE_OMITTED = new Map<string, string>([
  [
    "save_connections.expectedRevisions",
    "连接配置的 CAS 护栏（两个窗口同时改同一条连接时不互相覆盖）。" +
      "**本轮有意不接**：后端 save_connections 只返回 Result<(), String>，" +
      "不把落盘后的新 revision 告诉前端 ⇒ 前端学会不到新值，" +
      "第二次保存就会被自己的旧值判成冲突。要正确接，得先让后端返回新 revision，" +
      "或保存后重新 load 一次 —— 那是改「保存连接」主流程的独立一轮，不塞进本轮。",
  ],
]);

/* ────────────────────────── 门禁 ────────────────────────── */

const backend = backendCommands();
const registered = registeredCommands();
const structs = backendStructs();
const calls = frontendInvokes();
const calledCmds = new Set(calls.map((c) => c.cmd));

test("后端命令清单与注册表都不是空的（量具自身要有效）", () => {
  assert.ok(backend.size >= 40, `后端命令太少，解析器可能坏了: ${backend.size}`);
  assert.ok(registered.size >= 40, `注册表太小，解析器可能坏了: ${registered.size}`);
  assert.ok(calls.length >= 25, `前端调用点太少，扫描范围可能不对: ${calls.length}`);
});

test("前端 invoke 的每个命令，后端都真的注册了（点了必崩）", () => {
  const bad = calls
    .filter((c) => !registered.has(c.cmd))
    .map((c) => `${c.cmd}（${c.file}:${c.line}）`);
  assert.deepEqual(bad, [], `前端调用了后端没注册的命令: ${bad.join(", ")}`);
});

test("前端传的每个参数名，后端签名里都有（拼错 ⇒ 静默降级）", () => {
  const bad: string[] = [];
  for (const c of calls) {
    const cmd = backend.get(c.cmd);
    if (!cmd) continue;
    const allowed = new Set(cmd.params.filter((p) => !p.injected).map((p) => p.camel));
    for (const k of c.keys) {
      if (!allowed.has(k)) {
        bad.push(`${c.cmd} 传了后端不认的参数 ${k}（${c.file}:${c.line}）`);
      }
    }
  }
  assert.deepEqual(bad, [], bad.join("\n"));
});

test("后端的必填参数，前端都传了（漏传 ⇒ 运行时才发现）", () => {
  const bad: string[] = [];
  for (const c of calls) {
    const cmd = backend.get(c.cmd);
    if (!cmd) continue;
    const required = cmd.params.filter((p) => !p.injected && !p.optional).map((p) => p.camel);
    for (const r of required) {
      if (!c.keys.includes(r)) {
        bad.push(`${c.cmd} 漏传必填参数 ${r}（${c.file}:${c.line}）`);
      }
    }
  }
  assert.deepEqual(bad, [], bad.join("\n"));
});

test("后端注册的命令，要么被前端调用，要么在「预留未接线」清单里显式声明", () => {
  const unaccounted: string[] = [];
  for (const cmd of registered) {
    if (calledCmds.has(cmd)) continue;
    if (RESERVED_NOT_WIRED.has(cmd)) continue;
    unaccounted.push(cmd);
  }
  assert.deepEqual(
    unaccounted,
    [],
    `这些命令实现了、注册了，却既没有界面入口、也没登记为预留: ${unaccounted.join(", ")}`
  );
});

test("「预留未接线」清单：命令必须真的存在，且真的没被前端调用", () => {
  for (const [cmd, why] of RESERVED_NOT_WIRED) {
    assert.ok(why.length > 10, `${cmd} 的预留理由太短，等于没写`);
    assert.ok(registered.has(cmd), `${cmd} 后端已不存在 ⇒ 预留清单该删了`);
    assert.ok(!calledCmds.has(cmd), `${cmd} 已经被前端调用了 ⇒ 应从预留清单移除`);
  }
});

test("后端签名里没有 camelCase 混用的坑（参数名映射自洽）", () => {
  const bad: string[] = [];
  for (const [name, cmd] of backend) {
    const seen = new Set<string>();
    for (const p of cmd.params) {
      if (seen.has(p.camel)) bad.push(`${name} 的参数 ${p.snake} 与别的参数映射到同一个 ${p.camel}`);
      seen.add(p.camel);
    }
  }
  assert.deepEqual(bad, [], bad.join("\n"));
});

test("后端的可选参数，前端要么传、要么在「有意省略」清单里写明理由", () => {
  // 只看前端**真的调过**的命令：没调过的命令，它的参数压根没人关心。
  const omitted: string[] = [];
  const declared = new Set<string>();
  for (const c of calls) {
    const cmd = backend.get(c.cmd);
    if (!cmd) continue;
    for (const p of cmd.params) {
      if (p.injected || !p.optional) continue;
      if (c.keys.includes(p.camel)) continue;
      const key = `${c.cmd}.${p.camel}`;
      if (MAYBE_OMITTED.has(key)) {
        declared.add(key);
        continue;
      }
      omitted.push(key);
    }
  }
  assert.deepEqual(
    omitted,
    [],
    `这些可选参数前端一次都没传 ⇒ 功能静默降级，且没人会发现: ${omitted.join(", ")}`
  );
  // 清单里不能有已经传了的（接了就该删掉）
  for (const key of MAYBE_OMITTED.keys()) {
    const [cmd, param] = key.split(".");
    const call = calls.find((c) => c.cmd === cmd);
    assert.ok(call, `${key} 对应的命令前端根本没调过 ⇒ 清单该删了`);
    assert.ok(
      !call!.keys.includes(param),
      `${key} 前端已经传了 ⇒ 应从「有意省略」清单移除`
    );
  }
});

test("「有意省略」清单里的理由不能是空话", () => {
  for (const [key, why] of MAYBE_OMITTED) {
    assert.ok(why.length > 20, `${key} 的省略理由太短，等于没写`);
  }
});

/* ──────────────────── 嵌套结构体参数（serde 语义） ──────────────────── */

/**
 * 找出所有「前端传了对象字面量、后端参数类型是结构体」的调用点。
 *
 * 注意：**嵌套字段走 serde（snake_case），与 Tauri 顶层参数的 camelCase 不是一套规则**。
 * 拼错的代价是 serde 静默忽略 ⇒ 参数变 None ⇒ 功能静默降级。
 */
function nestedStructCalls(): Array<{
  call: FrontCall;
  key: string;
  structName: string;
  given: string[];
  fields: Set<string>;
  defaulted: Set<string>;
}> {
  const out: Array<{
    call: FrontCall;
    key: string;
    structName: string;
    given: string[];
    fields: Set<string>;
    defaulted: Set<string>;
  }> = [];
  for (const c of calls) {
    const cmd = backend.get(c.cmd);
    if (!cmd) continue;
    for (const [key, given] of c.nested) {
      const p = cmd.params.find((x) => x.camel === key);
      if (!p) continue;
      const name = structNameOf(p.ty);
      if (!name) continue;
      const st = structs.get(name);
      if (!st) continue;
      out.push({
        call: c,
        key,
        structName: name,
        given,
        fields: st.fields,
        defaulted: st.defaulted,
      });
    }
  }
  return out;
}

test("嵌套结构体字段：前端传的字段名后端得认（serde 是 snake_case，不是 camelCase）", () => {
  const bad: string[] = [];
  for (const n of nestedStructCalls()) {
    for (const k of n.given) {
      if (!n.fields.has(k)) {
        bad.push(
          `${n.call.cmd}.${n.key}.${k} 后端的 ${n.structName} 没这个字段` +
            `（serde 会静默忽略 ⇒ 参数变 None ⇒ 功能静默降级）`
        );
      }
    }
  }
  assert.deepEqual(bad, [], bad.join("\n"));
});

test("嵌套结构体的必填字段，前端都传了（漏传 ⇒ 运行时才发现）", () => {
  const bad: string[] = [];
  for (const n of nestedStructCalls()) {
    for (const f of n.fields) {
      if (n.defaulted.has(f)) continue;
      if (!n.given.includes(f)) {
        bad.push(
          `${n.call.cmd}.${n.key} 漏传必填字段 ${f}（${n.structName}）` +
            `（${n.call.file}:${n.call.line}）`
        );
      }
    }
  }
  assert.deepEqual(bad, [], bad.join("\n"));
});

test("嵌套结构体的可选字段，前端要么传、要么写明为什么不传", () => {
  // 可选字段漏传**不报错** ⇒ 后端按「调用方没要求」走 ⇒ 静默降级
  // （例如审批参数没送到 ⇒ 表现为「后端说只读模式拒绝导入」）。
  const bad: string[] = [];
  for (const n of nestedStructCalls()) {
    for (const f of n.fields) {
      if (!n.defaulted.has(f)) continue;
      if (n.given.includes(f)) continue;
      const key = `${n.call.cmd}.${n.key}.${f}`;
      if (MAYBE_OMITTED.has(key)) continue;
      bad.push(`${key} 一次都没传 ⇒ 功能静默降级，且没人会发现`);
    }
  }
  assert.deepEqual(bad, [], bad.join("\n"));
});

test("嵌套结构体这道门本身是有效的（量具不能是空跑）", () => {
  const ns = nestedStructCalls();
  assert.ok(ns.length >= 1, `没扫到嵌套结构体调用点，解析器可能坏了: ${ns.length}`);
  const names = ns.map((n) => `${n.call.cmd}.${n.key}:${n.structName}`);
  assert.ok(
    names.some((n) => n.includes("csv_import_execute.gate:ImportGate")),
    `CSV 导入门禁没被这道门覆盖到: ${names.join(", ")}`
  );
});
