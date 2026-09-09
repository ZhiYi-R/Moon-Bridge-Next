// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 无头模式：只提供带鉴权的网关服务，全程不碰 Tauri/WebView，不依赖桌面环境。
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--headless") {
        if args.iter().any(|a| a == "--help" || a == "-h") {
            println!("{}", moonbridge_app_lib::headless::usage());
            return;
        }
        let opts = match moonbridge_app_lib::headless::parse_args(&args) {
            Ok(opts) => opts,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        };
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("headless runtime 构建失败");
        if let Err(e) = rt.block_on(moonbridge_app_lib::headless::run_headless(opts)) {
            eprintln!("headless 运行失败: {e:#}");
            std::process::exit(1);
        }
        return;
    }

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
