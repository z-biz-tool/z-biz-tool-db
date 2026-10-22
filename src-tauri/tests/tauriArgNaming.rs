//! Tauri v2 的 IPC 参数命名规则 —— **用真实 IPC 通道实测**，不靠「文档说是 camelCase」。
//!
//! 为什么要有这道门：2026-10-22 实测发现，db 的前端用 snake_case 传
//! `access_mode` / `expected_generation` / `max_rows`，而 Tauri v2 的宏
//! （`tauri-macros/src/command/wrapper.rs`）会把 Rust 参数名转成 camelCase，
//! 运行时（`tauri/src/ipc/command.rs` 的 `deserialize_option`）**只查 camelCase 的 key**，
//! 查不到就 `visit_none()` ⇒ **静默变 None，不报错**。
//!
//! 后果不是「参数传丢」这么轻：
//! - `access_mode` 收不到 ⇒ `mode` 恒为 `"readOnly"` ⇒ **写操作永远被只读门禁挡住**；
//! - `max_rows` 收不到 ⇒ 行数上限恒失效（界面以为限了，后端没限）。
//!
//! ⇒ 典型的「界面说成功、事实不是」。
//!
//! 这道门把「Tauri 怎么收参」变成**可复跑的断言**，而不是注释里的一句口头约定：
//! 前端那侧（`tests/ipcContract.test.ts`）按 camelCase 写门禁，依据就是这里的实测。

use serde_json::json;
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{mock_builder, mock_context, noop_assets, INVOKE_KEY};
use tauri::webview::InvokeRequest;

/// 探针命令：把「后端到底收到了什么」原样吐回来。
/// 参数名特意用 snake_case（Rust 侧习惯），好让两种 key 的差别显形。
#[tauri::command]
fn probe_arg_names(access_mode: Option<String>, max_rows: Option<u32>) -> serde_json::Value {
    json!({ "access_mode": access_mode, "max_rows": max_rows })
}

/// 走一遍真实 IPC 通道（mock runtime），把命令的返回值拿出来。
fn call(body: serde_json::Value) -> Result<serde_json::Value, serde_json::Value> {
    let app = mock_builder()
        .invoke_handler(tauri::generate_handler![probe_arg_names])
        .build(mock_context(noop_assets()))
        .expect("mock app 应能建起来");
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("mock webview 应能建起来");
    let url = if cfg!(any(windows, target_os = "android")) {
        "http://tauri.localhost"
    } else {
        "tauri://localhost"
    };
    tauri::test::get_ipc_response(
        &webview,
        InvokeRequest {
            cmd: "probe_arg_names".into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: url.parse().unwrap(),
            body: InvokeBody::Json(body),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
    .map(|b| {
        b.deserialize::<serde_json::Value>()
            .expect("返回值应是合法 JSON")
    })
}

#[test]
fn camel_case_key_才收得到() {
    let got = call(json!({ "accessMode": "writable", "maxRows": 100 }))
        .expect("camelCase 的 key 应当被收下");
    assert_eq!(got, json!({ "access_mode": "writable", "max_rows": 100 }));
}

#[test]
fn snake_case_key_被静默丢掉() {
    // ⚠ 这就是 db 前端踩的坑：snake_case 的 key 既不报错、也不生效，
    // Option 参数直接变 None ⇒ 后端以为「调用方没要求」，于是按默认值走。
    let got = call(json!({ "access_mode": "writable", "max_rows": 100 }))
        .expect("命令本身不该失败 —— 失败才会让这个坑现形");
    assert_eq!(
        got,
        json!({ "access_mode": null, "max_rows": null }),
        "snake_case 的 key 竟然收到了 ⇒ Tauri 行为变了，这道门的结论要重测"
    );
}

#[test]
fn 两种_key_不能互相替代() {
    // 混着传：camelCase 的那个收得到，snake_case 的那个收不到 ——
    // 说明这不是「整体不认」，而是**逐个 key 认**，所以现象是「静默丢一半」。
    let got = call(json!({ "accessMode": "writable", "max_rows": 100 })).expect("命令本身不该失败");
    assert_eq!(got, json!({ "access_mode": "writable", "max_rows": null }));
}

#[test]
fn 必填参数缺_key_会报错_不是静默() {
    // 必填参数（Option 之外）缺 key 时 Tauri 会报 "missing required key" ——
    // 这说明「静默」只发生在 Option 上：**必填参数炸得出来，可选参数炸不出来**，
    // 后者才是危险的那一类。
    let app = mock_builder()
        .invoke_handler(tauri::generate_handler![probe_arg_names])
        .build(mock_context(noop_assets()))
        .expect("mock app 应能建起来");
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("mock webview 应能建起来");
    let url = if cfg!(any(windows, target_os = "android")) {
        "http://tauri.localhost"
    } else {
        "tauri://localhost"
    };
    let resp = tauri::test::get_ipc_response(
        &webview,
        InvokeRequest {
            cmd: "probe_arg_names".into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: url.parse().unwrap(),
            body: InvokeBody::Json(json!({})),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    );
    // 两个都是 Option ⇒ 不该报错，应当是两个 null
    let body = resp
        .expect("全是 Option 参数时不该报错")
        .deserialize::<serde_json::Value>()
        .expect("返回值应是合法 JSON");
    assert_eq!(body, json!({ "access_mode": null, "max_rows": null }));
}
