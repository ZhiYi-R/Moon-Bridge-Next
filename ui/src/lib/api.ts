// 前后端契约层：Tauri command 的类型安全封装 + DTO 类型定义。

import { invoke } from "@tauri-apps/api/core";

import { mockInvoke } from "./mock";

/** 当前是否运行在 Tauri 壳内（浏览器直接打开 vite dev 时为 false）。 */
export const isTauriRuntime =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** Mock 开关：非 Tauri 环境（纯浏览器开发）或 VITE_USE_MOCK=1 时启用。 */
export const usingMock =
  !isTauriRuntime || import.meta.env.VITE_USE_MOCK === "1";

/** command 分发：Tauri 壳内走真实 IPC，浏览器/强制 mock 时走内存 mock。 */
function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return usingMock ? mockInvoke<T>(cmd, args ?? {}) : invoke<T>(cmd, args);
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
  createdAt: number;
  updatedAt: number;
}

/** 模型元数据定义（仅承载模型自身属性；定价口径统一在 Offer）。 */
export interface ModelDef {
  slug: string;
  displayName?: string | null;
  contextWindow?: number | null;
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
  modalities: string[];
  reasoningLevels: string[];
  /** 定价（USD / 1M tokens），仅含有值项：input/output/cache_read/cache_write/reasoning。 */
  pricing: Record<string, number>;
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
  totalCost: number;
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

export const pluginApi = {
  list: () => call<PluginRecord[]>("plugin_list"),
  get: (name: string) => call<PluginRecord | null>("plugin_get", { name }),
  save: (plugin: PluginRecord) => call<void>("plugin_save", { plugin }),
  remove: (name: string) => call<void>("plugin_delete", { name }),
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
  query: (params: { model?: string; status?: string; limit?: number; offset?: number } = {}) =>
    call<UsageRecord[]>("usage_query", params),
  summary: () => call<UsageSummary>("usage_summary"),
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

/** 从 Tauri command 错误中提取可读消息（后端返回 `{ message }`）。 */
export function errMsg(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) return String((e as { message: unknown }).message);
  return String(e);
}
