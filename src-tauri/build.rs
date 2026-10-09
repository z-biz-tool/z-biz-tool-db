fn main() {
    // 图标不在 tauri-build 的重跑指纹里，不挂这两条就会把旧 resource.lib 链进新 exe
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/icon.icns");
    tauri_build::build()
}
