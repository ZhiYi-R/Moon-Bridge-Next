//! 真实网络冒烟（手工跑，不进 CI）：
//!   cargo test -p moonbridge-app --test real_login_smoke -- --ignored --nocapture
//! 依赖：本机真实 ~/.commandcode/auth.json（Command Code CLI 已登录）与外网。

use std::sync::Arc;

use moonbridge_app_lib::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "真实网络手工冒烟"]
async fn command_code_local_cli_import_real() {
    let home = dirs::home_dir().expect("home");
    if !home.join(".commandcode").join("auth.json").exists() {
        eprintln!("跳过：无 ~/.commandcode/auth.json");
        return;
    }
    let dir = std::env::temp_dir().join(format!("mb-smoke-{}", std::process::id()));
    let paths = config::AppPaths::resolve(dir.join("config"), dir.join("data"));
    let st = state::ManagedState::new(paths).unwrap();
    // 内置插件已种子；走真实 local-cli 来源（whoami 打真实端点）
    let out = run_begin_for_test(&st, "command-code-auth", Some("local-cli".into()))
        .await
        .expect("run_begin 应成功");
    assert!(out.already_done, "local-cli 命中应直出 done");
    let key = out.provider_key.clone().unwrap();
    assert_eq!(key, "command-code-auth");
    // 落库布局：provider（api_key 空、extra.auth 非敏感）+ secrets（令牌包）+ binding
    let p = st.db.get_provider(&key).unwrap().unwrap();
    assert_eq!(p.endpoints[0].api_key, "");
    assert!(
        p.extra["auth"]["account"].as_str().is_some(),
        "whoami 应解析出账户"
    );
    let raw = st
        .db
        .secret_get("provider:command-code-auth", "oauth")
        .unwrap()
        .expect("令牌包应已写入 secrets");
    let b: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(!b["access"].as_str().unwrap().is_empty());
    assert!(b.get("expires_at").is_none(), "长期 key 无 expires_at");
    assert!(st
        .db
        .plugin_enabled_in_scope("auth-commandcode", None, None, Some(&key))
        .unwrap());
    eprintln!("SMOKE OK: account={:?}", p.extra["auth"]["account"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "真实网络手工冒烟"]
async fn kimi_device_begin_real() {
    let dir = std::env::temp_dir().join(format!("mb-smoke-kimi-{}", std::process::id()));
    let paths = config::AppPaths::resolve(dir.join("config"), dir.join("data"));
    let st = state::ManagedState::new(paths).unwrap();
    // 设备码授权打真实 auth.kimi.com；拿到验证码即取消流程（不完成浏览器授权）
    let out = run_begin_for_test(&st, "kimi-oauth", None)
        .await
        .expect("Kimi 设备授权请求应成功（form 编码 + X-Msh 头被接受）");
    assert!(!out.already_done);
    assert!(out.user_code.as_deref().is_some_and(|c| !c.is_empty()));
    assert!(out
        .verification_url
        .as_deref()
        .is_some_and(|u| u.starts_with("https://")));
    eprintln!(
        "SMOKE OK: user_code={} url={}",
        out.user_code.unwrap(),
        out.verification_url.unwrap()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 复用 commands::oauth 的 run_begin（测试通道，避免走 Tauri State）。
async fn run_begin_for_test(
    st: &Arc<state::ManagedState>,
    preset: &str,
    source: Option<String>,
) -> Result<commands::oauth::OAuthBegin, commands::CommandError> {
    commands::oauth::run_begin(st, preset, source).await
}
