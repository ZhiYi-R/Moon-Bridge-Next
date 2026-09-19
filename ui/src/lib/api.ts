// 前后端契约层：Tauri command 的类型安全封装 + DTO 类型定义。

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
  createdAt: number;
  updatedAt: number;
}

/** 模型元数据定义（仅承载模型自身属性；定价口径统一在 Offer）。 */
export interface ModelDef {
  slug: string;
  displayName?: string | null;
  contextWindow?: number | null;
  /** 输出 token 上限（models.dev limit.output）；Anthropic 类上游在客户端未设上限时以此兜底。 */
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

// ───────────────────────── 余额看板 ─────────────────────────

/** 配额显示样式：`auto` 按数据自动判断，`percent`/`amount` 强制对应口径。 */
export type DisplayMode = "auto" | "percent" | "amount";

/** 余额卡片：key 即卡片名，scriptRef 为插件目录内的 `.lua` 路径或内联脚本原文。 */
export interface BalanceCard {
  key: string;
  /** 上游服务引用；无手动 Key 时解析其端点 Key，否则仅作分组和提供商标签。 */
  providerKey: string | null;
  /** 显示样式；后端落库时非法值归一为 `auto`。 */
  displayMode: DisplayMode;
  /** 手动 Key，可换行输入多个；去空白、保序去重后非空则完全覆盖上游服务 Key。 */
  apiKey: string;
  /** 查询 URL（可选）：配额接口基准地址，脚本里 `ctx.base_url` 读取；与 Provider 端点无关。 */
  baseUrl: string;
  /** 旧卡遗留的展示名；新卡片恒为空串，界面按 `providerKey` 展示并分组。 */
  providerLabel: string;
  scriptRef: string;
  /** 自动查询间隔（秒）；0 = 关闭定时。 */
  intervalSecs: number;
  enabled: boolean;
  extra: Json;
  position: number;
  createdAt: number;
  updatedAt: number;
}

/** 单条配额进度（used/left 缺一由前端按 100 互补）。 */
export interface BalanceQuota {
  label: string;
  usedPercent?: number | null;
  leftPercent?: number | null;
  /** 金额单位（如 `¥`、`$`、`GB`），仅金额模式使用。 */
  unit?: string | null;
  /** 已用金额；存在即走金额模式，不再展示百分比。 */
  usedAmount?: number | null;
  /** 剩余金额；缺失时金额模式只展示已用。 */
  leftAmount?: number | null;
  resetAt?: string | null;
}

/** 脚本返回的余额载荷（宽松结构：约定字段 + 任意附加键）。 */
export interface BalancePayload {
  status?: string;
  message?: string;
  quotas?: BalanceQuota[];
  summary?: string;
  [k: string]: Json;
}

export interface BalanceResult {
  status: "ok" | "error";
  payload?: BalancePayload | null;
  error?: string | null;
  /** 查询时刻（unix 秒）。 */
  queriedAt: number;
}

/** 单个 key 的查询结果：多 key 卡片按 key 拆行，前端在同一卡片内逐 key 渲染。 */
export interface BalanceKeyResult extends BalanceResult {
  /** key 在卡片解析结果中的序号（0 起）。 */
  keyIndex: number;
  /** 查询结果使用掩码标签（如 `sk-kim…LXyw`）；卡片编辑仍会接触原始 key。 */
  keyLabel: string;
}

export interface BalanceCardView extends BalanceCard {
  /** 逐 key 的最近一次查询结果（按 keyIndex 升序）；从未查询为空数组。 */
  results: BalanceKeyResult[];
}

export const balanceApi = {
  list: () => call<BalanceCardView[]>("balance_card_list"),
  save: (card: BalanceCard) => call<void>("balance_card_save", { card }),
  remove: (key: string) => call<void>("balance_card_delete", { key }),
  /** 刷新卡片：传 keyIndex 只重跑该 key，缺省整卡全量重跑。 */
  refresh: (key: string, keyIndex?: number) =>
    call<BalanceCardView>("balance_card_refresh", { key, keyIndex: keyIndex ?? null }),
  refreshAll: () => call<BalanceCardView[]>("balance_refresh_all"),
  /** dry-run：用表单里的卡片配置试跑一次脚本（逐 key 返回），不写库、不影响线上结果。 */
  test: (card: BalanceCard) => call<BalanceKeyResult[]>("balance_card_test", { card }),
};

/** 从 Tauri command 错误中提取可读消息（后端返回 `{ message }`）。 */
export function errMsg(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) return String((e as { message: unknown }).message);
  return String(e);
}
