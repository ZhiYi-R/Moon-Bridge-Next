//! 内置认证插件：随二进制分发（`include_str!`），首次启动种子进 db。
//!
//! 种子策略：仅在「同名插件不存在」时插入，此后不再触碰——用户可在 Plugins 页
//! 查看/编辑脚本（script_ref 为内联源码），自定义修改不会被覆盖。
//!
//! 插件记录 `enabled = false`（全局停用）：auth 插件按 provider 维度绑定生效，
//! 全局启用会让它对每个 provider 的出站都被解析一次（认证来源错位）。网关加载器
//! 对「全局停用但存在 enabled=1 绑定」的插件仍会加载，运行时门控精确到 provider。
//! 第三方平台新增认证：在 Plugins 页新建 capabilities 含 `auth` 的插件即可，
//! 不需要改任何 Rust 代码。

use moonbridge_store::{Database, PluginRecord};
use serde_json::{json, Value};

/// 内置插件表：(插件名, 脚本源码)。
const BUILTIN: &[(&str, &str)] = &[
    ("auth-kimi", include_str!("../../plugins/auth/kimi.lua")),
    (
        "auth-commandcode",
        include_str!("../../plugins/auth/commandcode.lua"),
    ),
];

/// 种子内置认证插件（幂等；插入失败仅记录错误，不阻断启动）。
pub fn seed_auth_plugins(db: &Database) {
    for (name, script) in BUILTIN {
        match db.get_plugin(name) {
            Ok(Some(_)) => {}
            Ok(None) => {
                let rec = PluginRecord {
                    name: (*name).to_string(),
                    source: "lua".to_string(),
                    script_ref: (*script).to_string(),
                    enabled: false,
                    config: json!({}),
                    scopes: vec!["provider".to_string()],
                    capabilities: vec!["auth".to_string()],
                    category: "core".to_string(),
                    config_schema: Value::Null,
                };
                if let Err(e) = db.upsert_plugin(&rec) {
                    tracing::error!(plugin = %name, error = %e, "内置认证插件种子失败");
                } else {
                    tracing::info!(plugin = %name, "内置认证插件已种子");
                }
            }
            Err(e) => tracing::error!(plugin = %name, error = %e, "读取插件记录失败，跳过种子"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_inserts_once_and_preserves_user_edits() {
        let db = Database::open_in_memory().unwrap();
        seed_auth_plugins(&db);
        for (name, script) in BUILTIN {
            let rec = db.get_plugin(name).unwrap().expect("种子应入库");
            assert!(!rec.enabled, "auth 插件应全局停用（provider 绑定生效）");
            assert_eq!(rec.script_ref, *script);
            assert_eq!(rec.capabilities, vec!["auth".to_string()]);
            assert_eq!(rec.scopes, vec!["provider".to_string()]);
        }
        // 二次种子：用户改名/改脚本不被覆盖
        let mut rec = db.get_plugin("auth-kimi").unwrap().unwrap();
        rec.script_ref = "-- user edited".to_string();
        db.upsert_plugin(&rec).unwrap();
        seed_auth_plugins(&db);
        assert_eq!(
            db.get_plugin("auth-kimi").unwrap().unwrap().script_ref,
            "-- user edited",
            "已有插件不得被种子覆盖"
        );
    }
}
