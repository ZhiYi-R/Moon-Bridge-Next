//! `mb.*` 宿主 API 注册与 Lua 沙箱。
//!
//! 只向插件暴露白名单能力：日志、config、会话状态、受控 HTTP（经 [`HostBridge`]）、
//! 跨 provider 调用、headers 辅助、crypto。危险的 `os/io/loadfile/dofile` 等在
//! [`sandbox`] 中移除。所有网络能力经 `HostBridge` 收口，由 gateway 施加 egress
//! proxy / 超时 / 域名白名单。

use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use hmac::{Hmac, Mac};
use mlua::{HookTriggers, Lua, LuaSerdeExt, Value as LuaValue, VmState};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::bridge::{HostBridge, HttpRequest};
use crate::error::Result;
use crate::quota::{ExecutionBudget, SandboxLimits};
use crate::session::SessionStore;

/// `mb.headers` 辅助（纯 Lua 实现，操作 `{{k,v},...}` 保序数组，大小写不敏感）。
const HEADERS_LUA: &str = r#"
mb.headers = {
  get = function(hs, name)
    if type(hs) ~= "table" then return nil end
    for i = 1, #hs do
      if type(hs[i]) == "table" and hs[i][1] and hs[i][1]:lower() == name:lower() then
        return hs[i][2]
      end
    end
    return nil
  end,
  set = function(hs, name, val)
    if type(hs) ~= "table" then hs = {} end
    for i = 1, #hs do
      if type(hs[i]) == "table" and hs[i][1] and hs[i][1]:lower() == name:lower() then
        hs[i][2] = val
        return hs
      end
    end
    hs[#hs + 1] = { name, val }
    return hs
  end,
  remove = function(hs, name)
    if type(hs) ~= "table" then return hs end
    for i = #hs, 1, -1 do
      if type(hs[i]) == "table" and hs[i][1] and hs[i][1]:lower() == name:lower() then
        table.remove(hs, i)
      end
    end
    return hs
  end,
}
"#;

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 移除危险标准库，并施加内存上限 + 指令计数/超时 hook，建立沙箱。
///
/// - 移除 `os / io / loadfile / dofile / package.loadlib`，切断文件与动态库访问；
/// - `set_memory_limit`：单状态内存硬上限，越界分配触发 `MemoryError`；
/// - `set_hook(every_nth_instruction)`：每 N 条指令检查一次执行预算
///   （[`ExecutionBudget`]），指令数或 wall-clock 超时越界即返回错误中止 Lua，
///   从而掐断死循环与超长计算。预算由宿主在每次钩子调用前重置。
pub fn sandbox(lua: &Lua, limits: &SandboxLimits, budget: &Arc<ExecutionBudget>) -> Result<()> {
    let g = lua.globals();
    for name in ["os", "io", "loadfile", "dofile"] {
        g.set(name, LuaValue::Nil)?;
    }
    if let Ok(pkg) = g.get::<mlua::Table>("package") {
        pkg.set("loadlib", LuaValue::Nil)?;
    }

    // 内存上限（模块模式/外部管理状态时不可用，降级为仅记录告警）
    if let Err(e) = lua.set_memory_limit(limits.max_memory_bytes) {
        tracing::warn!(error = %e, "设置 Lua 内存上限失败，跳过内存配额");
    }

    // 指令计数 + 超时 hook
    let step = limits.instruction_step.max(1);
    let b = budget.clone();
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(step),
        move |_lua, _debug| match b.charge(step as u64) {
            Ok(()) => Ok(VmState::Continue),
            Err(msg) => Err(mlua::Error::runtime(msg)),
        },
    );
    Ok(())
}

/// 注册 `mb.*` 宿主 API 到全局。
pub fn register(
    lua: &Lua,
    plugin_name: &str,
    config: &Value,
    host: Arc<dyn HostBridge>,
    sessions: SessionStore,
) -> Result<()> {
    let mb = lua.create_table()?;

    // ---- mb.log.{debug,info,warn,error} ----
    let log = lua.create_table()?;
    for level in ["debug", "info", "warn", "error"] {
        let n = plugin_name.to_string();
        let lv = level.to_string();
        let f = lua.create_function(move |_, msg: String| {
            match lv.as_str() {
                "debug" => tracing::debug!(target: "plugin", plugin = %n, "{msg}"),
                "info" => tracing::info!(target: "plugin", plugin = %n, "{msg}"),
                "warn" => tracing::warn!(target: "plugin", plugin = %n, "{msg}"),
                _ => tracing::error!(target: "plugin", plugin = %n, "{msg}"),
            }
            Ok(())
        })?;
        log.set(level, f)?;
    }
    mb.set("log", log)?;

    // ---- mb.config（静态注入）----
    mb.set("config", lua.to_value(config)?)?;

    // ---- mb.session.{get,set}（读全局 _MB_SESSION 定位当前会话）----
    let session = lua.create_table()?;
    {
        let s_get = sessions.clone();
        let n = plugin_name.to_string();
        let get = lua.create_function(move |lua, key: String| {
            let sid: Option<String> = lua.globals().get("_MB_SESSION").ok();
            match s_get.get(&n, sid.as_deref(), &key) {
                Some(v) => lua.to_value(&v),
                None => Ok(LuaValue::Nil),
            }
        })?;
        session.set("get", get)?;

        let s_set = sessions.clone();
        let n2 = plugin_name.to_string();
        let set = lua.create_function(move |lua, (key, val): (String, LuaValue)| {
            let sid: Option<String> = lua.globals().get("_MB_SESSION").ok();
            let json: Value = lua.from_value(val).unwrap_or(Value::Null);
            s_set.set(&n2, sid.as_deref(), &key, json);
            Ok(())
        })?;
        session.set("set", set)?;
    }
    mb.set("session", session)?;

    // ---- mb.http.request（async，经 HostBridge）----
    let http = lua.create_table()?;
    {
        let host_h = host.clone();
        let request = lua.create_async_function(move |lua, arg: LuaValue| {
            let host_h = host_h.clone();
            async move {
                let req: HttpRequest = lua.from_value(arg)?;
                let resp = host_h.http_request(req).await.map_err(mlua::Error::external)?;
                lua.to_value(&resp)
            }
        })?;
        http.set("request", request)?;
    }
    mb.set("http", http)?;

    // ---- mb.provider.invoke（async，跨 provider 编排）----
    let provider = lua.create_table()?;
    {
        let host_p = host.clone();
        let invoke = lua.create_async_function(
            move |lua, (prov, model, req_v): (String, String, LuaValue)| {
                let host_p = host_p.clone();
                async move {
                    let core_req: moonbridge_core::CoreRequest = lua.from_value(req_v)?;
                    let resp = host_p
                        .provider_invoke(&prov, &model, core_req)
                        .await
                        .map_err(mlua::Error::external)?;
                    lua.to_value(&resp)
                }
            },
        )?;
        provider.set("invoke", invoke)?;
    }
    mb.set("provider", provider)?;

    // ---- mb.crypto.{sha256,hmac_sha256,base64_encode,base64_decode} ----
    let crypto = lua.create_table()?;
    crypto.set(
        "sha256",
        lua.create_function(|_, s: String| {
            let mut h = Sha256::new();
            h.update(s.as_bytes());
            Ok(hex(&h.finalize()))
        })?,
    )?;
    crypto.set(
        "hmac_sha256",
        lua.create_function(|_, (key, msg): (String, String)| {
            let mut mac =
                Hmac::<Sha256>::new_from_slice(key.as_bytes()).map_err(mlua::Error::external)?;
            mac.update(msg.as_bytes());
            Ok(hex(&mac.finalize().into_bytes()))
        })?,
    )?;
    crypto.set(
        "base64_encode",
        lua.create_function(|_, s: String| Ok(B64.encode(s.as_bytes())))?,
    )?;
    crypto.set(
        "base64_decode",
        lua.create_function(|_, s: String| {
            let bytes = B64.decode(s.as_bytes()).map_err(mlua::Error::external)?;
            Ok(String::from_utf8_lossy(&bytes).to_string())
        })?,
    )?;
    mb.set("crypto", crypto)?;

    lua.globals().set("mb", mb)?;

    // ---- mb.headers（纯 Lua 辅助，需在 mb 就位后加载）----
    lua.load(HEADERS_LUA).set_name("mb_headers").exec()?;

    Ok(())
}
