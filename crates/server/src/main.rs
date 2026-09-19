//! `moonbridge-server` 入口：解析参数 → 初始化日志 → 运行服务端。

use tracing_subscriber::EnvFilter;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match moonbridge_server::args::parse_args(&args) {
        Ok(opts) => opts,
        Err(e) => {
            // `--help` 的文本也走这条 Err 通道（与 headless 一致），按用法输出。
            if args.iter().any(|a| a == "--help" || a == "-h") {
                println!("{e}");
                return;
            }
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("服务端 runtime 构建失败");
    if let Err(e) = rt.block_on(moonbridge_server::serve::run(opts)) {
        eprintln!("服务端运行失败: {e:#}");
        std::process::exit(1);
    }
}
