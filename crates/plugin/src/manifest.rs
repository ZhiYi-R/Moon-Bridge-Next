//! 插件清单（manifest）。
//!
//! 约定：每个 Lua 插件脚本执行后，在全局暴露一个 `MB` table，既承载清单元数据，
//! 也承载钩子函数。清单字段用于作用域过滤与能力门控（尤其是报文层钩子是否触发，
//! 直接决定是否产生每-chunk 的 Lua 调用开销）。

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 能力：Core IR 语义层钩子。
pub const CAP_CORE: &str = "core";
/// 能力：入站/出站请求报文钩子。
pub const CAP_RAW_REQUEST: &str = "raw_request";
/// 能力：入站/出站响应报文钩子。
pub const CAP_RAW_RESPONSE: &str = "raw_response";
/// 能力：流式 SSE chunk 报文钩子（高频，未声明则完全跳过）。
pub const CAP_RAW_STREAM: &str = "raw_stream";

/// 插件清单。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// 唯一插件名。
    pub name: String,
    /// 版本。
    #[serde(default = "default_version")]
    pub version: String,
    /// 作用域：global / provider / model / route。
    #[serde(default)]
    pub scopes: Vec<String>,
    /// 声明的能力集合。
    #[serde(default)]
    pub capabilities: HashSet<String>,
    /// 配置 JSON Schema（供 UI 渲染表单，可选）。
    #[serde(default)]
    pub config_schema: Option<Value>,
    /// 入口提示（可选）。
    #[serde(default)]
    pub entry: Option<String>,
}

fn default_version() -> String {
    "0.0.0".to_string()
}

impl Manifest {
    /// 是否声明某能力。
    pub fn has(&self, cap: &str) -> bool {
        self.capabilities.contains(cap)
    }
    /// 是否需要 Core 层钩子。
    pub fn needs_core(&self) -> bool {
        self.has(CAP_CORE)
    }
    /// 是否需要请求报文钩子。
    pub fn needs_raw_request(&self) -> bool {
        self.has(CAP_RAW_REQUEST)
    }
    /// 是否需要响应报文钩子。
    pub fn needs_raw_response(&self) -> bool {
        self.has(CAP_RAW_RESPONSE)
    }
    /// 是否需要流式 chunk 报文钩子。
    pub fn needs_raw_stream(&self) -> bool {
        self.has(CAP_RAW_STREAM)
    }
}
