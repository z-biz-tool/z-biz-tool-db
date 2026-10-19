/**
 * TLS 档位的前后端契约（T-025）。
 *
 * 跑法：node --experimental-strip-types --test tests/tlsModeContract.test.ts
 *
 * 为什么要这道门：TLS 档位是**安全语义**，前端下拉的值必须与后端 serde 的
 * 值逐字相同，也必须与后端映射到 sqlx 的档位一一对应。
 * 任何一侧改了名字或含义，界面就会显示"校验证书"而实际不校验 ——
 * 那比没有这个功能更坏。
 *
 * 本仓的 `environment` 字段（unknown/dev/test/...）也是同一类"名字在两侧各写一遍"的问题。
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const ROOT = fileURLToPath(new URL("..", import.meta.url)).replace(/\/+$/, "");
const APP = readFileSync(`${ROOT}/src/App.tsx`, "utf8");
const LIB = readFileSync(`${ROOT}/src-tauri/src/lib.rs`, "utf8");

/** 后端 `TlsMode` 枚举的变体（serde rename_all = "snake_case"） */
const BACKEND_VARIANTS = ["VerifyFull", "Prefer", "Disable"];

test("后端枚举的每个变体都在前端下拉里有对应项", () => {
  for (const v of BACKEND_VARIANTS) {
    // serde 的 snake_case：VerifyFull → verify_full（不带前导下划线）
    const snake = v.replace(/([A-Z])/g, "_$1").toLowerCase().replace(/^_/, "");
    assert.ok(
      APP.includes(`"${snake}"`),
      `前端缺少后端枚举 ${v} 对应的档位 "${snake}"`
    );
  }
});

test("前端下拉的三个档位与后端一一对应，不多不少", () => {
  const block = APP.slice(APP.indexOf("TLS_MODE_OPTIONS"));
  const values = [...block.slice(0, block.indexOf("];")).matchAll(/value: "(\w+)"/g)].map(
    (m) => m[1]
  );
  assert.deepEqual(
    [...values].sort(),
    ["disable", "prefer", "verify_full"],
    "前端档位集合与后端不一致"
  );
});

test("降级档位在界面上必须写明「不校验证书」", () => {
  const block = APP.slice(APP.indexOf("TLS_MODE_OPTIONS"));
  // prefer 这一项的提示文字里必须出现"不校验"三个字 —— 否则用户以为它更安全
  const prefer = block.slice(block.indexOf('value: "prefer"'), block.indexOf('value: "disable"'));
  assert.ok(
    prefer.includes("不校验"),
    "prefer 档位的说明里必须写明「不校验证书」，否则用户会以为它比校验更安全"
  );
  const disable = block.slice(block.indexOf('value: "disable"'));
  assert.ok(
    disable.includes("明文"),
    "disable 档位的说明里必须写明「明文」"
  );
});

test("后端三档都真的映射到了 sqlx 的 SSL 模式（不是 no-op）", () => {
  // 旧实现三个 match 分支全是 `opts`（no-op）⇒ 档位形同虚设。
  // 守卫：三个变体各自都有一处明确的 to_mysql / to_pg 映射。
  for (const v of BACKEND_VARIANTS) {
    assert.ok(
      LIB.includes(`TlsMode::${v} => MySqlSslMode::`),
      `TlsMode::${v} 缺少 MySQL 映射`
    );
    assert.ok(
      LIB.includes(`TlsMode::${v} => PgSslMode::`),
      `TlsMode::${v} 缺少 PostgreSQL 映射`
    );
  }
});

test("verify_full 档位映射到「会验主机名」的那一档，不是 sqlx 的默认值", () => {
  // sqlx 的默认是 Preferred/Prefer（不校验证书）。旧实现"留 opts 默认"，
  // 等于宣称"默认强制校验"却根本不校验。这条守住它不再退化。
  assert.ok(
    LIB.includes("TlsMode::VerifyFull => MySqlSslMode::VerifyIdentity"),
    "verify_full 必须映射到 VerifyIdentity（验 CA + 验主机名）"
  );
  assert.ok(
    LIB.includes("TlsMode::VerifyFull => PgSslMode::VerifyFull"),
    "verify_full 必须映射到 PgSslMode::VerifyFull"
  );
});

test("配置透传：前后端字段名对得上（tls_mode / tls_ca_path）", () => {
  assert.ok(
    APP.includes("tls_mode: c.tlsMode ?? null"),
    "前端 toBackendConfig 必须把 tlsMode 传成后端的 tls_mode"
  );
  assert.ok(
    APP.includes("tls_ca_path: c.tlsCaPath ?? null"),
    "前端 toBackendConfig 必须把 tlsCaPath 传成后端的 tls_ca_path"
  );
  assert.ok(
    LIB.includes("pub tls_mode: Option<TlsMode>"),
    "后端 DBConfig 缺少 tls_mode 字段"
  );
  assert.ok(
    LIB.includes("pub tls_ca_path: Option<String>"),
    "后端 DBConfig 缺少 tls_ca_path 字段"
  );
});

test("私有 CA 通道存在：内置根证书集是公共 CA，私有 CA 必须能显式给文件", () => {
  assert.ok(LIB.includes(".ssl_ca("), "MySQL 侧缺少 ssl_ca 私有 CA 通道");
  assert.ok(LIB.includes(".ssl_root_cert("), "PG 侧缺少 ssl_root_cert 私有 CA 通道");
});
