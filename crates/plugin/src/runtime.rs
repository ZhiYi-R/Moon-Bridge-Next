//! Lua 插件运行时：加载脚本、暴露 `MB` 表中的钩子、按引用语义调用。
//!
//! 并发模型：每插件一个 `mlua::Lua`（启用 `send`），以 `Arc<tokio::Mutex<Lua>>`
//! 串行化访问，允许在持有 Lua 时跨 await 执行异步宿主调用。Core IR / Raw 报文经
//! [`crate::convert`] 与 Lua table 互转；利用 table 引用语义，插件就地修改后由
//! 宿主从同一 table 回写，返回值仅表达动作（short_circuit/abort/drop）。

use std::sync::Arc;

use mlua::{
    FromLuaMulti, Function, HookTriggers, IntoLuaMulti, Lua, LuaSerdeExt, Table, Value as LuaValue,
    VmState,
};
use tokio::sync::Mutex;

use moonbridge_core::{ContentBlock, CoreRequest, CoreResponse, CoreStreamEvent, Tool};
use moonbridge_protocol::{ChunkVerdict, RawChunk, RawMessage, RawVerdict, ReqCtx};

use crate::bridge::HostBridge;
use crate::convert;
use crate::error::{PluginError, Result};
use crate::host;
use crate::manifest::Manifest;
use crate::quota::{ExecutionBudget, SandboxLimits};
use crate::session::SessionStore;

/// 单个 Lua 插件的运行时。
pub struct LuaRuntime {
    /// 插件名（来自 store 配置，权威）。
    pub name: String,
    /// 从脚本 `MB` 表解析出的清单。
    pub manifest: Manifest,
    /// 插件全局开关（store `plugins.enabled`）。Provider 维度的三态覆盖
    /// 由 [`LuaPluginRegistry`] 的 bindings 门控表决定。
    pub enabled: bool,
    lua: Arc<Mutex<Lua>>,
    /// 沙箱配额上限。
    limits: SandboxLimits,
    /// 每次钩子调用前重置的执行预算（指令计数 + 截止时间）。
    budget: Arc<ExecutionBudget>,
}

/// 从全局 `MB` 表取钩子函数（不存在则 None）。
fn mb_fn(lua: &Lua, name: &str) -> Option<Function> {
    let mb: Table = lua.globals().get("MB").ok()?;
    mb.get::<Option<Function>>(name).ok().flatten()
}

/// 每次钩子调用前，把当前会话写入全局，供 `mb.session` 定位。
fn prepare(lua: &Lua, ctx: &ReqCtx) -> Result<()> {
    lua.globals().set("_MB_SESSION", ctx.session_id.clone())?;
    Ok(())
}

impl LuaRuntime {
    /// 以默认沙箱配额加载并初始化一个插件运行时。
    pub fn new(
        name: &str,
        script: &str,
        config: &serde_json::Value,
        host: Arc<dyn HostBridge>,
        sessions: SessionStore,
    ) -> Result<Self> {
        Self::new_with_limits(name, script, config, host, sessions, SandboxLimits::default())
    }

    /// 以指定沙箱配额加载并初始化一个插件运行时。
    pub fn new_with_limits(
        name: &str,
        script: &str,
        config: &serde_json::Value,
        host: Arc<dyn HostBridge>,
        sessions: SessionStore,
        limits: SandboxLimits,
    ) -> Result<Self> {
        let lua = Lua::new();
        let budget = ExecutionBudget::new(limits.max_instructions);
        host::sandbox(&lua, &limits, &budget)?;
        host::register(&lua, name, config, host, sessions)?;
        // 加载脚本前重置预算，使脚本顶层的死循环/超长计算亦受指令与超时约束
        budget.begin(limits.call_timeout);
        lua.load(script)
            .set_name(name)
            .exec()
            .map_err(|e| PluginError::Runtime {
                name: name.to_string(),
                message: e.to_string(),
            })?;
        // 脚本加载完毕后移除主线程 hook；后续钩子调用的配额约束绑定到各自的协程
        lua.remove_hook();
        let manifest = Self::read_manifest(&lua, name)?;
        Ok(LuaRuntime {
            name: name.to_string(),
            manifest,
            // 默认启用；加载方（gateway）按 store 的 plugins.enabled 覆写
            enabled: true,
            lua: Arc::new(Mutex::new(lua)),
            limits,
            budget,
        })
    }

    /// 加锁 + 写入会话 + 重置本次调用的执行预算。
    async fn enter(&self, ctx: &ReqCtx) -> Result<tokio::sync::MutexGuard<'_, Lua>> {
        let lua = self.lua.lock().await;
        prepare(&lua, ctx)?;
        self.budget.begin(self.limits.call_timeout);
        Ok(lua)
    }

    /// 在受配额约束的协程上调用插件函数。
    ///
    /// `Function::call_async` 会在内部新建协程执行，而 Lua debug hook 是**按线程**
    /// 生效的——绑定在主线程的 hook 不会在协程内触发。故此处显式 `create_thread`
    /// 并把指令计数/超时 hook 绑定到该协程，再 `into_async` 驱动，确保异步调用
    /// （含插件内 `mb.http.request` 等 await）同样受配额约束。
    async fn call_hook<A, R>(&self, lua: &Lua, f: Function, args: A) -> Result<R>
    where
        A: IntoLuaMulti,
        R: FromLuaMulti,
    {
        let thread = lua.create_thread(f)?;
        let b = self.budget.clone();
        let step = self.limits.instruction_step.max(1);
        thread.set_hook(
            HookTriggers::new().every_nth_instruction(step),
            move |_lua, _debug| match b.charge(step as u64) {
                Ok(()) => Ok(VmState::Continue),
                Err(msg) => Err(mlua::Error::runtime(msg)),
            },
        );
        Ok(thread.into_async::<R>(args).await?)
    }

    /// 从 `MB` 表解析清单；缺失字段回退默认。
    fn read_manifest(lua: &Lua, name: &str) -> Result<Manifest> {
        let fallback = Manifest {
            name: name.to_string(),
            version: "0.0.0".to_string(),
            scopes: Vec::new(),
            capabilities: Default::default(),
            config_schema: None,
            entry: None,
        };
        let Ok(mb) = lua.globals().get::<Table>("MB") else {
            return Ok(fallback);
        };
        let version: Option<String> = mb.get("version").ok().flatten();
        let scopes: Vec<String> = mb.get("scopes").unwrap_or_default();
        let caps: Vec<String> = mb.get("capabilities").unwrap_or_default();
        let entry: Option<String> = mb.get("entry").ok().flatten();
        let config_schema = match mb.get::<LuaValue>("config_schema") {
            Ok(LuaValue::Nil) | Err(_) => None,
            Ok(v) => lua.from_value::<serde_json::Value>(v).ok(),
        };
        Ok(Manifest {
            name: name.to_string(),
            version: version.unwrap_or_else(|| "0.0.0".to_string()),
            scopes,
            capabilities: caps.into_iter().collect(),
            config_schema,
            entry,
        })
    }

    /// 调用插件 `init()`（若定义）。
    pub async fn init(&self) -> Result<()> {
        let lua = self.lua.lock().await;
        self.budget.begin(self.limits.call_timeout);
        if let Some(f) = mb_fn(&lua, "init") {
            let _: LuaValue = self.call_hook(&lua, f, ()).await?;
        }
        Ok(())
    }

    /// 调用插件 `shutdown()`（若定义）。
    pub async fn shutdown(&self) -> Result<()> {
        let lua = self.lua.lock().await;
        self.budget.begin(self.limits.call_timeout);
        if let Some(f) = mb_fn(&lua, "shutdown") {
            let _: LuaValue = self.call_hook(&lua, f, ()).await?;
        }
        Ok(())
    }

    // ── Core IR 语义层 ──────────────────────────────────────────────

    /// on_request：修改 CoreRequest。
    pub async fn on_request(&self, ctx: &ReqCtx, req: &mut CoreRequest) -> Result<()> {
        let lua = self.enter(ctx).await?;
        let Some(f) = mb_fn(&lua, "on_request") else {
            return Ok(());
        };
        let ctx_v = convert::ctx_to_lua(&lua, ctx)?;
        let req_v = lua.to_value(&*req)?;
        let ret: LuaValue = self.call_hook(&lua, f, (ctx_v, req_v.clone())).await?;
        let source = if matches!(ret, LuaValue::Table(_)) { ret } else { req_v };
        *req = lua.from_value(source)?;
        Ok(())
    }

    /// inject_tools：返回追加的工具。
    pub async fn inject_tools(&self, ctx: &ReqCtx) -> Result<Vec<Tool>> {
        let lua = self.enter(ctx).await?;
        let Some(f) = mb_fn(&lua, "inject_tools") else {
            return Ok(Vec::new());
        };
        let ctx_v = convert::ctx_to_lua(&lua, ctx)?;
        let ret: LuaValue = self.call_hook(&lua, f, ctx_v).await?;
        match ret {
            LuaValue::Table(_) => Ok(lua.from_value::<Vec<Tool>>(ret)?),
            _ => Ok(Vec::new()),
        }
    }

    /// on_response：修改 CoreResponse。
    pub async fn on_response(&self, ctx: &ReqCtx, resp: &mut CoreResponse) -> Result<()> {
        let lua = self.enter(ctx).await?;
        let Some(f) = mb_fn(&lua, "on_response") else {
            return Ok(());
        };
        let ctx_v = convert::ctx_to_lua(&lua, ctx)?;
        let resp_v = lua.to_value(&*resp)?;
        let ret: LuaValue = self.call_hook(&lua, f, (ctx_v, resp_v.clone())).await?;
        let source = if matches!(ret, LuaValue::Table(_)) { ret } else { resp_v };
        *resp = lua.from_value(source)?;
        Ok(())
    }

    /// on_stream_event：返回 true 表示丢弃；也可就地修改事件。
    pub async fn on_stream_event(&self, ctx: &ReqCtx, ev: &mut CoreStreamEvent) -> Result<bool> {
        let lua = self.enter(ctx).await?;
        let Some(f) = mb_fn(&lua, "on_stream_event") else {
            return Ok(false);
        };
        let ctx_v = convert::ctx_to_lua(&lua, ctx)?;
        let ev_v = lua.to_value(&*ev)?;
        let ret: LuaValue = self.call_hook(&lua, f, (ctx_v, ev_v.clone())).await?;
        match ret {
            LuaValue::Boolean(b) => {
                *ev = lua.from_value(ev_v)?;
                Ok(b)
            }
            LuaValue::Table(_) => {
                *ev = lua.from_value(ret)?;
                Ok(false)
            }
            _ => {
                *ev = lua.from_value(ev_v)?;
                Ok(false)
            }
        }
    }

    /// filter_content：返回 true 表示跳过该内容块。
    pub async fn filter_content(&self, ctx: &ReqCtx, block: &mut ContentBlock) -> Result<bool> {
        let lua = self.enter(ctx).await?;
        let Some(f) = mb_fn(&lua, "filter_content") else {
            return Ok(false);
        };
        let ctx_v = convert::ctx_to_lua(&lua, ctx)?;
        let blk_v = lua.to_value(&*block)?;
        let ret: LuaValue = self.call_hook(&lua, f, (ctx_v, blk_v.clone())).await?;
        match ret {
            LuaValue::Boolean(b) => {
                *block = lua.from_value(blk_v)?;
                Ok(b)
            }
            LuaValue::Table(_) => {
                *block = lua.from_value(ret)?;
                Ok(false)
            }
            _ => {
                *block = lua.from_value(blk_v)?;
                Ok(false)
            }
        }
    }

    /// transform_error：转换错误消息。
    pub async fn transform_error(&self, ctx: &ReqCtx, msg: &str) -> Result<String> {
        let lua = self.enter(ctx).await?;
        let Some(f) = mb_fn(&lua, "transform_error") else {
            return Ok(msg.to_string());
        };
        let ctx_v = convert::ctx_to_lua(&lua, ctx)?;
        let msg_v = lua.to_value(msg)?;
        let ret: LuaValue = self.call_hook(&lua, f, (ctx_v, msg_v)).await?;
        match ret {
            LuaValue::String(s) => Ok(s.to_str()?.to_string()),
            _ => Ok(msg.to_string()),
        }
    }

    // ── 出入站原始报文层 ────────────────────────────────────────────

    async fn call_raw_message(
        &self,
        fname: &str,
        ctx: &ReqCtx,
        msg: &mut RawMessage,
    ) -> Result<RawVerdict> {
        let lua = self.enter(ctx).await?;
        let Some(f) = mb_fn(&lua, fname) else {
            return Ok(RawVerdict::Pass);
        };
        let ctx_v = convert::ctx_to_lua(&lua, ctx)?;
        let msg_t = convert::raw_message_to_lua(&lua, msg, self.limits.max_body_bytes)?;
        let ret: LuaValue = self
            .call_hook(&lua, f, (ctx_v, LuaValue::Table(msg_t.clone())))
            .await?;
        // 插件就地修改的报文回写
        convert::apply_lua_to_message(&lua, &msg_t, msg)?;
        // 返回值表达短路/中止动作
        if let Some(v) = convert::parse_raw_action(&lua, ret)? {
            return Ok(v);
        }
        Ok(RawVerdict::Pass)
    }

    async fn call_raw_chunk(
        &self,
        fname: &str,
        ctx: &ReqCtx,
        chunk: &mut RawChunk,
    ) -> Result<ChunkVerdict> {
        let lua = self.enter(ctx).await?;
        let Some(f) = mb_fn(&lua, fname) else {
            return Ok(ChunkVerdict::Forward);
        };
        let ctx_v = convert::ctx_to_lua(&lua, ctx)?;
        let chunk_t = convert::raw_chunk_to_lua(&lua, chunk, self.limits.max_body_bytes)?;
        let ret: LuaValue = self
            .call_hook(&lua, f, (ctx_v, LuaValue::Table(chunk_t.clone())))
            .await?;
        convert::apply_lua_to_chunk(&lua, &chunk_t, chunk)?;
        Ok(convert::parse_chunk_action(ret))
    }

    /// on_client_request_raw。
    pub async fn on_client_request_raw(
        &self,
        ctx: &ReqCtx,
        msg: &mut RawMessage,
    ) -> Result<RawVerdict> {
        self.call_raw_message("on_client_request_raw", ctx, msg).await
    }
    /// on_upstream_request_raw。
    pub async fn on_upstream_request_raw(
        &self,
        ctx: &ReqCtx,
        msg: &mut RawMessage,
    ) -> Result<RawVerdict> {
        self.call_raw_message("on_upstream_request_raw", ctx, msg).await
    }
    /// on_upstream_response_raw。
    pub async fn on_upstream_response_raw(
        &self,
        ctx: &ReqCtx,
        msg: &mut RawMessage,
    ) -> Result<RawVerdict> {
        self.call_raw_message("on_upstream_response_raw", ctx, msg).await
    }
    /// on_client_response_raw。
    pub async fn on_client_response_raw(
        &self,
        ctx: &ReqCtx,
        msg: &mut RawMessage,
    ) -> Result<RawVerdict> {
        self.call_raw_message("on_client_response_raw", ctx, msg).await
    }
    /// on_upstream_chunk_raw。
    pub async fn on_upstream_chunk_raw(
        &self,
        ctx: &ReqCtx,
        chunk: &mut RawChunk,
    ) -> Result<ChunkVerdict> {
        self.call_raw_chunk("on_upstream_chunk_raw", ctx, chunk).await
    }
    /// on_client_chunk_raw。
    pub async fn on_client_chunk_raw(
        &self,
        ctx: &ReqCtx,
        chunk: &mut RawChunk,
    ) -> Result<ChunkVerdict> {
        self.call_raw_chunk("on_client_chunk_raw", ctx, chunk).await
    }
}
