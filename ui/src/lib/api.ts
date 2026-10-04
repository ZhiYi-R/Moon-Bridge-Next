// 前后端接口层：Tauri command 的类型安全封装 + DTO 类型定义。

import { invoke } from "@tauri-apps/api/core";

import { mockInvoke } from "./mock";
import { webInvoke } from "./web";

/** 当前是否运行在 Tauri 壳内（浏览器直接打开 vite dev 时为 false）。 */
export const isTauriRuntime =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** Mock 开关：非 Tauri 环境显式指定 VITE_USE_MOCK=1 时启用。 */
export const usingMock =
  !isTauriRuntime && import.meta.env.VITE_USE_MOCK === "1";

/** web 模式：浏览器中运行且未强制 mock，command 改走同源 /api/* 管理接口。 */
export const isWebRuntime = !isTauriRuntime && !usingMock;

/** command 分发：Tauri 壳内走真实 IPC，强制 mock 走内存 mock，其余走 web HTTP。 */
function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauriRuntime) return invoke<T>(cmd, args);
  if (usingMock) return mockInvoke<T>(cmd, args ?? {});
  return webInvoke<T>(cmd, args ?? {});
}

// ───────────────────────── DTO 类型（camelCase，匹配后端 serde 输出）─────────────────────────

export type Json = unknown;

export interface ProviderEndpoint {
  /** 上游协议；同一 Provider 可混合不同协议的端点 */
  protocol: string;
  baseUrl: string;
  apiKey: string;
}

export interface Provider {
  key: string;
  /** 端点列表（按序故障转移；每端点独立协议与 API Key，Key 留空沿用前一非空 Key） */
  endpoints: ProviderEndpoint[];
  version?: string | null;
  userAgent?: string | null;
  webSearch?: Json;
  extra: Json;
  enabled: boolean;
  /** 配额查询插件引用（category=quota 的插件名）；空串 = 未绑定配额查询。 */
  quotaPluginRef: string;
  /** 配额定时查询间隔（秒）；0 = 只手动刷新。 */
  quotaIntervalSecs: number;
  /** 配额查询开关（独立于 quotaPluginRef，便于临时停用）。 */
  quotaEnabled: boolean;
  /** 配额插件实例配置（按插件 configSchema 的字段值；密钥类字段也在其中，整体加密写入数据库）。 */
  quotaConfig: Json;
  createdAt: number;
  updatedAt: number;
}

/** 模型元数据定义（仅承载模型自身属性；定价口径统一在 Offer）。 */
export interface ModelDef {
  slug: string;
  displayName?: string | null;
  contextWindow?: number | null;
  /** 输出 token 上限（models.dev limit.output）；Anthropic 类上游在客户端未设上限时以此回退。 */
  maxOutputTokens?: number | null;
  modalities?: Json;
  reasoningLevels?: Json;
  extra: Json;
}

export interface Offer {
  providerKey: string;
  modelSlug: string;
  pricing?: Json;
  /** 绑定到 provider 的特定协议端点（如 "openai-response"）；空/缺省表示用全部端点故障转移。 */
  endpointProtocol?: string | null;
}

/** models.dev 候选模型（后端解析后的扁平视图，供搜索勾选导入）。 */
export interface CatalogModel {
  providerKey: string;
  providerName: string;
  id: string;
  name?: string | null;
  contextWindow?: number | null;
  /** 输出 token 上限（models.dev limit.output）。 */
  maxOutputTokens?: number | null;
  modalities: string[];
  reasoningLevels: string[];
  /** 定价（USD / 1M tokens）：input/output/cache_read/cache_write/reasoning 扁平价键，
   *  另可含长上下文分层 `tiers`/`context_over_200k`（对象值，非 number）。 */
  pricing: Record<string, Json>;
}

export interface Route {
  alias: string;
  modelSlug: string;
  providerKey: string;
  extra: Json;
}

export interface PluginRecord {
  name: string;
  source: string;
  scriptRef: string;
  enabled: boolean;
  config: Json;
  scopes: string[];
  capabilities: string[];
  /** 插件类别：`core` 请求链路插件（进钩子注册表）| `quota` 配额查询插件（绑定 Provider）。 */
  category?: string;
  /** 实例配置字段声明（MB.config_schema）：{ 字段名: { type, label, default, secret, help } }；quota 插件用它驱动 Provider 配额配置表单。 */
  configSchema?: Json;
}

export interface PluginBinding {
  pluginName: string;
  scope: string;
  scopeKey: string;
  enabled: boolean;
  config: Json;
}

export interface UsageRecord {
  id: string;
  sessionId?: string | null;
  model?: string | null;
  upstreamModel?: string | null;
  /** 命中的 provider key（定价粒度是 provider+model）；路由前失败为 null */
  providerKey?: string | null;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
  reasoningTokens: number;
  cost: number;
  status?: string | null;
  error?: string | null;
  latencyMs: number;
  /** 首字延迟（毫秒）；仅流式请求有值，非流式/错误为 null */
  ttftMs?: number | null;
  createdAt: number;
}

export interface UsageSummary {
  requests: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
  reasoningTokens: number;
  totalCost: number;
}

export interface ProviderCost {
  providerKey: string;
  cost: number;
  requests: number;
}

export interface Setting {
  key: string;
  value: Json;
}

/** trace 列表项（轻量元数据）。 */
export interface TraceEntry {
  session: string;
  model: string;
  fileName: string;
  relPath: string;
  modifiedAt: number;
  size: number;
}

/** 单条 trace 的完整快照（后端 camelCase，报文体为任意 JSON）。 */
export interface TraceDetail {
  requestId: string;
  createdAt: number;
  sessionId?: string | null;
  modelAlias: string;
  upstreamModel: string;
  providerKey: string;
  clientProtocol: string;
  upstreamProtocol: string;
  stream: boolean;
  status: string;
  latencyMs: number;
  /** 首字延迟（毫秒）；仅流式请求有值，非流式 / 插件代答为 null。 */
  ttftMs?: number | null;
  /** 插件在本请求上实际触发的上游重试次数（0/缺省 = 未重试；旧 trace 无该字段）。 */
  retries?: number;
  usage: {
    inputTokens: number;
    outputTokens: number;
    cacheReadTokens: number;
    cacheWriteTokens: number;
    reasoningTokens: number;
  };
  clientRequest: Json;
  upstreamRequest: Json;
  upstreamResponse: Json;
  clientResponse: Json;
  error?: string | null;
}

export interface GatewayStatus {
  running: boolean;
  addr: string;
  error?: string | null;
}

export interface AppInfo {
  version: string;
  dbPath: string;
  configPath: string;
  dataDir: string;
  pluginsDir: string;
  traceDir: string;
  /** 运行模式：server（web 服务端）等；Tauri 桌面端不上报。 */
  mode?: string;
}

export interface GatewayConfig {
  addr: string;
  authToken?: string | null;
  egressProxy?: string | null;
  maxBodyBytes: number;
  /** 插件脚本目录：script_ref 的相对 `.lua` 路径归一到此处，绝对路径须在其内。
   *  由 app 层按数据目录回填，设置页不编辑但须原样回传。 */
  pluginsDir?: string | null;
  requestTimeoutSecs: number;
  traceDir?: string | null;
  /** trace 是否记录请求/响应体；关闭时只留元数据。变更后需重启网关生效。 */
  traceRecordBodies?: boolean;
  /** trace 保留条数（按 mtime 保留最近 N 条，0 = 不清理）。变更后需重启网关生效。 */
  traceRetention?: number;
  /** 会话水印：往助手纯文本输出末尾附 [mb:xxxxxx] 标记以识别会话。变更后需重启网关生效。 */
  sessionMarker?: boolean;
  /** 会话活跃表容量（FIFO 淘汰深度）。设置页不编辑但必须原样回传——
   *  漏传会被序列化成 0 并经 save 落回默认值。 */
  sessionTableDepth?: number;
}

export interface AppConfig {
  gateway: GatewayConfig;
  logLevel: string;
  autoStart: boolean;
}

// ───────────────────────── command 封装（按领域分组）─────────────────────────

export const gatewayApi = {
  start: () => call<GatewayStatus>("gateway_start"),
  stop: () => call<GatewayStatus>("gateway_stop"),
  restart: () => call<GatewayStatus>("gateway_restart"),
  status: () => call<GatewayStatus>("gateway_status"),
};

export const providerApi = {
  list: () => call<Provider[]>("provider_list"),
  get: (key: string) => call<Provider | null>("provider_get", { key }),
  save: (provider: Provider) => call<void>("provider_save", { provider }),
  remove: (key: string) => call<void>("provider_delete", { key }),
};

export const modelApi = {
  list: () => call<ModelDef[]>("model_list"),
  get: (slug: string) => call<ModelDef | null>("model_get", { slug }),
  save: (model: ModelDef) => call<void>("model_save", { model }),
  remove: (slug: string) => call<void>("model_delete", { slug }),
  offerList: (providerKey: string) => call<Offer[]>("offer_list", { providerKey }),
  offerSave: (offer: Offer) => call<void>("offer_save", { offer }),
  offerRemove: (providerKey: string, modelSlug: string) =>
    call<void>("offer_delete", { providerKey, modelSlug }),
};

export const catalogApi = {
  /** 从 models.dev 拉取全部候选模型（后端解析精简）。 */
  fetch: () => call<CatalogModel[]>("catalog_fetch"),
  /** 把勾选的候选批量导入本地 models 表（含定价），返回导入数量。 */
  import: (models: CatalogModel[]) => call<number>("catalog_import", { models }),
};

export const routeApi = {
  list: () => call<Route[]>("route_list"),
  get: (alias: string) => call<Route | null>("route_get", { alias }),
  save: (route: Route) => call<void>("route_save", { route }),
  remove: (alias: string) => call<void>("route_delete", { alias }),
};

/** 单个插件的导入结果（逐文件）。 */
export interface PluginImportOutcome {
  path: string;
  name: string;
  /** imported / skipped / error */
  status: string;
  message?: string | null;
}

export const pluginApi = {
  list: () => call<PluginRecord[]>("plugin_list"),
  get: (name: string) => call<PluginRecord | null>("plugin_get", { name }),
  save: (plugin: PluginRecord) => call<void>("plugin_save", { plugin }),
  remove: (name: string) => call<void>("plugin_delete", { name }),
  /** 从磁盘路径批量导入 .lua 插件（仅 Tauri；同名插件跳过）。 */
  import: (paths: string[]) => call<PluginImportOutcome[]>("plugin_import", { paths }),
  readScript: (name: string) => call<string>("plugin_read_script", { name }),
  writeScript: (name: string, content: string) =>
    call<void>("plugin_write_script", { name, content }),
  bindingList: (pluginName: string) => call<PluginBinding[]>("binding_list", { pluginName }),
  bindingSave: (binding: PluginBinding) => call<void>("binding_save", { binding }),
  bindingListByScope: (scope: string) =>
    call<PluginBinding[]>("binding_list_by_scope", { scope }),
  bindingRemove: (pluginName: string, scope: string, scopeKey: string) =>
    call<void>("binding_delete", { pluginName, scope, scopeKey }),
};

export const usageApi = {
  query: (
    params: {
      model?: string;
      providerKey?: string;
      status?: string;
      since?: number;
      until?: number;
      limit?: number;
      offset?: number;
    } = {},
  ) => call<UsageRecord[]>("usage_query", params),
  summary: (params: { since?: number; until?: number } = {}) =>
    call<UsageSummary>("usage_summary", params),
  costByProvider: (since?: number) =>
    call<ProviderCost[]>("usage_cost_by_provider", { since }),
};

export const traceApi = {
  list: (limit?: number) => call<TraceEntry[]>("trace_list", { limit }),
  read: (relPath: string) => call<TraceDetail>("trace_read", { relPath }),
  remove: (relPath: string) => call<void>("trace_delete", { relPath }),
};

export const settingsApi = {
  get: (key: string) => call<Json>("settings_get", { key }),
  set: (key: string, value: Json) => call<void>("settings_set", { key, value }),
  list: () => call<Setting[]>("settings_list"),
  remove: (key: string) => call<void>("settings_delete", { key }),
};

export const appApi = {
  info: () => call<AppInfo>("app_info"),
  getConfig: () => call<AppConfig>("config_get"),
  setConfig: (config: AppConfig) => call<void>("config_set", { config }),
};

// ───────────────────────── 配额查询 ─────────────────────────

/** 配额类型判别：`percentage` 百分比额度 | `quota` 金额额度 | `counter` 计数器（不渲染）。 */
export type QuotaType = "percentage" | "quota" | "counter";

/** 单条配额（type 必填的判别式契约；后端已做 percent 互补，前端不再推断）。 */
export interface QuotaEntry {
  type: QuotaType;
  label: string;
  /** 配额窗口时长（秒）；有滚动窗口的配额填写，供本地消耗统计对照。 */
  periodSecs?: number | null;
  usedPercent?: number | null;
  leftPercent?: number | null;
  /** 金额单位（如 `¥`、`$`、`GB`），quota/counter 使用。 */
  unit?: string | null;
  usedAmount?: number | null;
  leftAmount?: number | null;
  resetAt?: string | null;
}

/** 配额脚本返回的载荷（宽松结构：约定字段 + 任意附加键）。 */
export interface QuotaPayload {
  status?: string;
  message?: string;
  quotas?: QuotaEntry[];
  summary?: string;
  /** 后端归一时剔除的非法配额原因。 */
  warnings?: string[];
  [k: string]: Json;
}

export interface QuotaResult {
  status: "ok" | "error";
  payload?: QuotaPayload | null;
  error?: string | null;
  /** 查询时刻（unix 秒）。 */
  queriedAt: number;
}

/** 单个端点 key 的查询结果：多端点 Provider 按 keyIndex（端点下标）拆行渲染。 */
export interface QuotaKeyResult extends QuotaResult {
  /** 端点序号（provider.endpoints 下标，0 起）。 */
  keyIndex: number;
  /** 查询结果使用掩码标签（如 `sk-kim…LXyw`）；Provider 编辑仍可接触原始 key。 */
  keyLabel: string;
}

/** Provider 配额视图：绑定信息 + 逐端点的最近一次查询结果。 */
export interface ProviderQuotaView {
  providerKey: string;
  /** 绑定的配额插件名。 */
  quotaPluginRef: string;
  /** 定时查询间隔（秒）；0 = 只手动刷新。 */
  quotaIntervalSecs: number;
  /** 配额查询开关。 */
  quotaEnabled: boolean;
  /** 端点数量（key 行数上限）。 */
  keyCount: number;
  /** 逐端点的最近一次查询结果（按 keyIndex 升序）；从未查询为空数组。 */
  results: QuotaKeyResult[];
}

export const quotaApi = {
  list: () => call<ProviderQuotaView[]>("quota_list"),
  /** 刷新一个 Provider 的配额（逐端点全量重跑）。 */
  refresh: (providerKey: string) =>
    call<ProviderQuotaView>("quota_refresh", { providerKey }),
  refreshAll: () => call<ProviderQuotaView[]>("quota_refresh_all"),
  /** dry-run：用表单里的 Provider 配置试跑一次配额脚本（逐端点返回），不写入数据库。 */
  test: (provider: Provider) => call<QuotaKeyResult[]>("quota_test", { provider }),
};

/** 从 Tauri command 错误中提取可读消息（后端返回 `{ message }`）。 */
export function errMsg(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) return String((e as { message: unknown }).message);
  return String(e);
}
