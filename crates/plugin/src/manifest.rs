//! 插件清单（manifest）。
//!
//! 约定：每个 Lua 插件脚本执行后，在全局暴露一个 `MB` table，既承载清单元数据，
//! 也承载钩子函数。清单字段用于作用域过滤与能力过滤（尤其是报文层钩子是否触发，
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
/// 能力：认证钩子（auth_describe/auth_begin/auth_poll/auth_refresh/auth_headers）。
/// provider 作用域绑定生效；不进入报文链路，由宿主按 provider 解析后显式调用。
pub const CAP_AUTH: &str = "auth";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    /// 插件类别：`core`（请求链路插件，进钩子注册表）| `quota`（配额查询插件，
    /// 绑定 Provider 由配额引擎驱动，不接触请求链路）。
    #[serde(default = "default_category")]
    pub category: String,
    /// 作用域：global / provider / model / route。
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub capabilities: HashSet<String>,
    /// 配置 JSON Schema（供 UI 渲染表单，可选）。
    #[serde(default)]
    pub config_schema: Option<Value>,
    #[serde(default)]
    pub entry: Option<String>,
    /// `mb.fs.read` 可读路径白名单（home 相对，`~` 开头；精确匹配，不支持通配）。
    /// 未声明即完全禁止插件读文件。
    #[serde(default)]
    pub fs_read_allow: Vec<String>,
}

fn default_version() -> String {
    "0.0.0".to_string()
}

fn default_category() -> String {
    "core".to_string()
}

impl Manifest {
    pub fn has(&self, cap: &str) -> bool {
        self.capabilities.contains(cap)
    }
    pub fn needs_core(&self) -> bool {
        self.has(CAP_CORE)
    }
    pub fn needs_raw_request(&self) -> bool {
        self.has(CAP_RAW_REQUEST)
    }
    pub fn needs_raw_response(&self) -> bool {
        self.has(CAP_RAW_RESPONSE)
    }
    pub fn needs_raw_stream(&self) -> bool {
        self.has(CAP_RAW_STREAM)
    }
}
