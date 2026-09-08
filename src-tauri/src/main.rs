// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Linux：规避 WebKitGTK 在部分环境（DMA-BUF 渲染器 / 合成模式）下的白屏、
    // resize 崩溃与指针事件丢失问题。参考 tauri-apps/tauri#9394。
    #[cfg(target_os = "linux")]
    {
        if std::env::var("WEBKIT_DISABLE_DMABUF_RENDERER").is_err() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
        if std::env::var("WEBKIT_DISABLE_COMPOSITING_MODE").is_err() {
            std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
        }
    }

    moonbridge_app_lib::run();
}
