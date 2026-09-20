//! `mb.*` 宿主 API 注册与 Lua 沙箱。
//!
//! 只向插件暴露白名单能力：日志、config、会话状态、受控 HTTP（经 [`HostBridge`]）、
//! 跨 provider 调用、headers 辅助、crypto。危险的 `os/io/loadfile/dofile/require/package`
//! 等在 [`sandbox`] 中移除。所有网络能力经 `HostBridge` 收口，由 gateway 施加 egress
//! 代理与兜底超时（注意：目前**没有**域名白名单实现，出站目标不受限）。

use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as B64, engine::general_purpose::URL_SAFE_NO_PAD as B64URL, Engine as _};
use hmac::{Hmac, Mac};
use mlua::{HookTriggers, Lua, LuaSerdeExt, Thread, Value as LuaValue, VmState};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::bridge::{CallbackSpec, HostBridge, HttpRequest};
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

/// 危险全局：切断文件、动态库与模块加载访问。
///
/// mlua `Lua::new()` 打开的是 `StdLib::ALL_SAFE`，**含 `io`/`os`/`package`**，它们被
/// 注册进 `package.loaded`（即 registry 里的 `_LOADED`）。因此只置空 `_G.os`/`_G.io`
/// 是不够的——插件里 `require("io")` 会直接取回活表，`io.open(path,"w")` 随即成立。
/// 光删 `require` 也不够：`package.searchers` 的元素本身就能返回**可执行 loader**
/// （等价 `loadfile`）。故连 `require` 与整个 `package` 表一并移除。
///
/// `load`/`loadstring` 一并移除：切断运行时把字符串编译成代码的能力（纵深防御，
/// 配合已移除的 `debug` 杜绝重新取得执行钩子 / 原生 loader 的路径）。
///
/// 已确认 `plugins/**`、余额模板与 crate 内测试均不使用这些名字；LSP 桩
/// （`plugins/moonbridge.lua`）本就声明「沙箱中 os/io/loadfile/dofile 不可用」。
const DANGEROUS_GLOBALS: [&str; 9] = [
    "os", "io", "loadfile", "dofile", "require", "package", "debug", "load", "loadstring",
];

/// 协程钩子补丁：把宿主装的指令计数/超时钩子传导到插件新建的线程。
///
/// Lua 的 debug hook 按 `lua_State`（线程）生效且**不被新建协程继承**，所以宿主在
/// `create_thread` 上挂的那颗钩子只覆盖它自己那一层；插件内
/// `coroutine.wrap(function() while true do end end)()` 会在一个全新、无钩子的线程里
/// 死循环，绕过 `max_instructions` 与 `call_timeout`，并因 `Lua` 互斥守卫跨 await
/// 持有而永久卡死该插件的请求链路。
///
/// 做法：包一层 `coroutine.create`/`wrap`，线程创建后立即经宿主回调补挂钩子。
/// 两者都必须经**同一个 upvalue 构造函数**拿线程，不能让 `wrap` 去查全局
/// `coroutine.create`——否则插件覆写 `coroutine.create` 就能让 `wrap` 造出无钩线程。
/// `bind` 同样必须是 upvalue 而非全局，否则插件 `__mb_bind_thread = nil` 即可绕过。
/// 原生 `coroutine.wrap` 在 C 内直连线程创建、不经过 Lua 层 `create`，故须一并覆写；
/// 其多返回值与错误传播语义用 `table.pack/unpack` 复刻。
const COROUTINE_PATCH: &str = r#"
local bind = __mb_bind_thread
__mb_bind_thread = nil
local _create, _resume = coroutine.create, coroutine.resume
local function new_thread(f)
  local co = _create(f)
  bind(co)
  return co
end
coroutine.create = new_thread
coroutine.wrap = function(f)
  local co = new_thread(f)
  return function(...)
    local r = table.pack(_resume(co, ...))
    if not r[1] then error(r[2]) end
    return table.unpack(r, 2, r.n)
  end
end
"#;

/// 移除危险标准库，并施加内存上限 + 指令计数/超时 hook，建立沙箱。
///
/// - 移除 [`DANGEROUS_GLOBALS`]，切断文件、动态库与模块加载访问；
/// - 施加 [`COROUTINE_PATCH`]，使配额钩子覆盖插件自建协程；
/// - `set_memory_limit`：单状态内存硬上限，越界分配触发 `MemoryError`；
/// - `set_hook(every_nth_instruction)`：每 N 条指令检查一次执行预算
///   （[`ExecutionBudget`]），指令数或 wall-clock 超时越界即返回错误中止 Lua，
///   从而掐断死循环与超长计算。预算由宿主在每次钩子调用前重置。
pub fn sandbox(lua: &Lua, limits: &SandboxLimits, budget: &Arc<ExecutionBudget>) -> Result<()> {
    let g = lua.globals();
    for name in DANGEROUS_GLOBALS {
        g.set(name, LuaValue::Nil)?;
    }

    // 内存上限（模块模式/外部管理状态时不可用，降级为仅记录告警）
    if let Err(e) = lua.set_memory_limit(limits.max_memory_bytes) {
        tracing::warn!(error = %e, "设置 Lua 内存上限失败，跳过内存配额");
    }

    // 指令计数 + 超时 hook（主线程）
    let step = limits.instruction_step.max(1);
    let b = budget.clone();
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(step),
        move |_lua, _debug| match b.charge(step as u64) {
            Ok(()) => Ok(VmState::Continue),
            Err(msg) => Err(mlua::Error::runtime(msg)),
        },
    );

    // 同一份预算补挂到插件新建的每个协程线程
    let binder = {
        let b = budget.clone();
        lua.create_function(move |_lua, thread: Thread| {
            let b = b.clone();
            thread.set_hook(
                HookTriggers::new().every_nth_instruction(step),
                move |_, _| match b.charge(step as u64) {
                    Ok(()) => Ok(VmState::Continue),
                    Err(msg) => Err(mlua::Error::runtime(msg)),
                },
            );
            Ok(())
        })?
    };
    g.set("__mb_bind_thread", binder)?;
    lua.load(COROUTINE_PATCH)
        .set_name("mb_sandbox_coroutine")
        .exec()?;
    // 补丁内已把 bind 收为 upvalue 并清除全局名；此处再兜底一次
    g.set("__mb_bind_thread", LuaValue::Nil)?;

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
                let resp = host_h
                    .http_request(req)
                    .await
                    .map_err(mlua::Error::external)?;
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
    // base64url（无填充）：JWT payload 解码等
    crypto.set(
        "base64url_decode",
        lua.create_function(|_, s: String| {
            let bytes = B64URL.decode(s.as_bytes()).map_err(mlua::Error::external)?;
            Ok(String::from_utf8_lossy(&bytes).to_string())
        })?,
    )?;
    crypto.set(
        "base64url_encode",
        lua.create_function(|_, s: String| Ok(B64URL.encode(s.as_bytes())))?,
    )?;
    mb.set("crypto", crypto)?;

    // ---- mb.time.now_ms（沙箱无 os 库；过期时刻换算需要墙钟）----
    {
        let time = lua.create_table()?;
        time.set(
            "now_ms",
            lua.create_function(|_, ()| {
                Ok(std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0))
            })?,
        )?;
        mb.set("time", time)?;
    }

    // ---- mb.secret.{get,set,delete}（加密小值；scope 强制按插件命名空间隔离）----
    // OAuth 令牌包不经过这里——它由宿主在登录编排与出站链路之间直传（见 auth 引擎），
    // Lua 侧只以函数参数形态收到自己平台产出的 bundle。
    let secret = lua.create_table()?;
    {
        let n = plugin_name.to_string();
        let h = host.clone();
        secret.set(
            "get",
            lua.create_async_function(move |_, (scope, key): (String, String)| {
                let h = h.clone();
                let scope = format!("{n}/{scope}");
                async move { h.secret_get(&scope, &key).await.map_err(mlua::Error::external) }
            })?,
        )?;
        let n = plugin_name.to_string();
        let h = host.clone();
        secret.set(
            "set",
            lua.create_async_function(move |_, (scope, key, value): (String, String, String)| {
                let h = h.clone();
                let scope = format!("{n}/{scope}");
                async move {
                    h.secret_set(&scope, &key, &value)
                        .await
                        .map_err(mlua::Error::external)
                }
            })?,
        )?;
        let n = plugin_name.to_string();
        let h = host.clone();
        secret.set(
            "delete",
            lua.create_async_function(move |_, (scope, key): (String, String)| {
                let h = h.clone();
                let scope = format!("{n}/{scope}");
                async move {
                    h.secret_delete(&scope, &key)
                        .await
                        .map_err(mlua::Error::external)
                }
            })?,
        )?;
    }
    mb.set("secret", secret)?;

    // ---- mb.random.{state,bytes}（CSPRNG；沙箱内没有安全随机源）----
    let random = lua.create_table()?;
    {
        let h = host.clone();
        random.set(
            "state",
            lua.create_function(move |_, ()| {
                let bytes = h.random_bytes(16).map_err(mlua::Error::external)?;
                Ok(hex(&bytes))
            })?,
        )?;
        let h = host.clone();
        random.set(
            "bytes",
            lua.create_function(move |lua, n: usize| {
                if n == 0 || n > 4096 {
                    return Err(mlua::Error::external("mb.random.bytes 长度须在 1..=4096"));
                }
                let bytes = h.random_bytes(n).map_err(mlua::Error::external)?;
                lua.create_string(&bytes)
            })?,
        )?;
    }
    mb.set("random", random)?;

    // ---- mb.oauth.{listen_callback,callback_await,callback_close} ----
    // 回环监听由宿主托管：插件不能自己 bind 端口；一次性、带超时、完成即清理。
    let oauth = lua.create_table()?;
    {
        let h = host.clone();
        oauth.set(
            "listen_callback",
            lua.create_async_function(move |lua, spec_v: LuaValue| {
                let h = h.clone();
                async move {
                    let spec: CallbackSpec = lua.from_value(spec_v)?;
                    validate_callback_spec(&spec).map_err(mlua::Error::external)?;
                    let handle = h
                        .oauth_listen_callback(spec)
                        .await
                        .map_err(mlua::Error::external)?;
                    lua.to_value(&handle)
                }
            })?,
        )?;
        let h = host.clone();
        oauth.set(
            "callback_await",
            lua.create_async_function(move |lua, (id, timeout_ms): (String, Option<u64>)| {
                let h = h.clone();
                async move {
                    // 单次等待上限 30s：更长的心跳由插件在 Lua 侧自旋，
                    // 避免一个失控 await 长期占住宿主监听表
                    let wait = timeout_ms.unwrap_or(1000).min(30_000);
                    match h.oauth_callback_await(&id, wait).await {
                        Ok(Some(v)) => lua.to_value(&v),
                        Ok(None) => Ok(LuaValue::Nil),
                        Err(e) => Err(mlua::Error::external(e)),
                    }
                }
            })?,
        )?;
        let h = host.clone();
        oauth.set(
            "callback_close",
            lua.create_async_function(move |_, id: String| {
                let h = h.clone();
                async move {
                    h.oauth_callback_close(&id)
                        .await
                        .map_err(mlua::Error::external)
                }
            })?,
        )?;
    }
    mb.set("oauth", oauth)?;

    // ---- mb.fs.read（白名单制；白名单在调用时从 MB.fs_read_allow 动态读取）----
    {
        let h = host.clone();
        let fs = lua.create_table()?;
        fs.set(
            "read",
            lua.create_async_function(move |lua, path: String| {
                let h = h.clone();
                async move {
                    let resolved = resolve_fs_whitelist(&lua, &path)
                        .map_err(mlua::Error::external)?;
                    h.fs_read(&resolved, FS_READ_MAX_BYTES)
                        .await
                        .map_err(mlua::Error::external)
                }
            })?,
        )?;
        mb.set("fs", fs)?;
    }

    // ---- mb.open_external（仅 http/https，防 file:// 等 scheme 滥用）----
    {
        let h = host.clone();
        mb.set(
            "open_external",
            lua.create_async_function(move |_, url: String| {
                let h = h.clone();
                async move {
                    let lower = url.trim_start().to_ascii_lowercase();
                    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
                        return Err(mlua::Error::external(
                            "mb.open_external 仅允许 http/https URL",
                        ));
                    }
                    h.open_external(&url).await.map_err(mlua::Error::external)
                }
            })?,
        )?;
    }

    lua.globals().set("mb", mb)?;

    // ---- mb.headers（纯 Lua 辅助，需在 mb 就位后加载）----
    lua.load(HEADERS_LUA).set_name("mb_headers").exec()?;

    Ok(())
}

/// `mb.fs.read` 单文件大小上限（凭据文件量级）。
const FS_READ_MAX_BYTES: u64 = 256 * 1024;

/// 回调监听 spec 边界校验（路径形态与 origins 数量/形态）。
fn validate_callback_spec(spec: &CallbackSpec) -> std::result::Result<(), String> {
    let p = spec.path.trim();
    if !p.starts_with('/')
        || p.len() > 64
        || !p
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/-_".contains(c))
    {
        return Err(format!("回调路径形态非法: {p}"));
    }
    if spec.origins.len() > 8 {
        return Err("CORS 允许源过多（上限 8）".to_string());
    }
    for o in &spec.origins {
        let lower = o.to_ascii_lowercase();
        if !(lower.starts_with("https://") || lower.starts_with("http://")) || o.len() > 256 {
            return Err(format!("CORS 允许源形态非法: {o}"));
        }
    }
    Ok(())
}

/// 归一化 home 相对路径：必须以 `~/` 开头；剔除 `.`、拒绝 `..`、折叠分隔符。
fn normalize_home_relative(raw: &str) -> Option<Vec<String>> {
    let t = raw.trim();
    let rest = t.strip_prefix("~/").or_else(|| t.strip_prefix("~\\"))?;
    let mut parts = Vec::new();
    for seg in rest.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => return None,
            s => parts.push(s.to_string()),
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts)
}

/// 路径相等比较：Windows 大小写不敏感，其它平台精确。
fn path_eq(a: &[String], b: &[String]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(x, y)| {
        #[cfg(windows)]
        {
            x.eq_ignore_ascii_case(y)
        }
        #[cfg(not(windows))]
        {
            x == y
        }
    })
}

/// 校验请求路径命中 `MB.fs_read_allow` 白名单，返回解析后的绝对路径。
///
/// 双向归一（白名单条目与请求都必须 `~/` 开头、无 `..`），精确匹配逐段比较，
/// 不做前缀/通配——白名单即「这一份文件」，最小暴露面。白名单在调用时从 MB 表
/// 动态读取：register() 先于脚本执行，此刻 MB 尚不存在。
fn resolve_fs_whitelist(lua: &Lua, request: &str) -> std::result::Result<String, String> {
    let req = normalize_home_relative(request)
        .ok_or_else(|| format!("路径必须是 home 相对（~/...）且无 ..: {request}"))?;
    let allow: Vec<String> = lua
        .globals()
        .get::<mlua::Table>("MB")
        .and_then(|mb| mb.get("fs_read_allow"))
        .unwrap_or_default();
    let home = dirs::home_dir().ok_or("无法定位 home 目录")?;
    for entry in allow {
        if let Some(allow_parts) = normalize_home_relative(&entry) {
            if path_eq(&req, &allow_parts) {
                let mut abs = home.clone();
                for p in &req {
                    abs.push(p);
                }
                return Ok(abs.to_string_lossy().to_string());
            }
        }
    }
    Err(format!("路径不在 fs_read_allow 白名单内: {request}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_relative_normalization() {
        assert_eq!(
            normalize_home_relative("~/.commandcode/auth.json"),
            Some(vec![".commandcode".to_string(), "auth.json".to_string()])
        );
        // 分隔符折叠与 . 剔除
        assert_eq!(
            normalize_home_relative("~\\.commandcode/./auth.json"),
            Some(vec![".commandcode".to_string(), "auth.json".to_string()])
        );
        // 越界/绝对路径/裸相对路径一律拒绝
        assert!(normalize_home_relative("~/../etc/passwd").is_none());
        assert!(normalize_home_relative("~/.commandcode/../../x").is_none());
        assert!(normalize_home_relative("/etc/passwd").is_none());
        assert!(normalize_home_relative(".commandcode/auth.json").is_none());
        assert!(normalize_home_relative("~").is_none());
        assert!(normalize_home_relative("~/").is_none());
    }

    #[test]
    fn callback_spec_validation() {
        let mut spec = CallbackSpec {
            path: "/callback".to_string(),
            origins: vec!["https://commandcode.ai".to_string()],
            preferred_port: Some(5959),
        };
        assert!(validate_callback_spec(&spec).is_ok());
        spec.path = "callback".to_string();
        assert!(validate_callback_spec(&spec).is_err());
        spec.path = "/call back?x=1".to_string();
        assert!(validate_callback_spec(&spec).is_err());
        spec.path = "/callback".to_string();
        spec.origins = vec!["javascript:alert(1)".to_string()];
        assert!(validate_callback_spec(&spec).is_err());
        spec.origins = (0..9).map(|i| format!("https://o{i}.example.com")).collect();
        assert!(validate_callback_spec(&spec).is_err());
    }
}
