// 浏览器开发模式的内存 Mock 层：模拟后端 Tauri command 行为。
//
// 触发条件见 api.ts 的 `usingMock`（非 Tauri 环境，或 VITE_USE_MOCK=1）。
// 所有数据驻留内存，刷新页面即重置；CRUD 写操作会真实反映到后续读操作，
// 因此前端各管理页在纯浏览器中即可完整走通交互流程。

import type {
  AppConfig,
  AppInfo,
  GatewayConfig,
  GatewayStatus,
  Json,
  ModelDef,
  Offer,
  PluginBinding,
  PluginRecord,
  Provider,
  Route,
  TraceDetail,
  TraceEntry,
  UsageRecord,
  UsageSummary,
} from "./api";

const now = Date.now();
const MIN = 60_000;
const HOUR = 60 * MIN;

/** 模拟 IPC 延迟，让 loading 状态在开发时可观察。 */
const delay = (ms = 60) => new Promise<void>((r) => setTimeout(r, ms));

// ───────────────────────── 种子数据 ─────────────────────────

const seedProviders: Provider[] = [
  {
    key: "anthropic-official",
    endpoints: [
      { protocol: "anthropic", baseUrl: "https://api.anthropic.com", apiKey: "sk-ant-mock-000111222333" },
      { protocol: "anthropic", baseUrl: "https://mirror.anthropic.com", apiKey: "sk-ant-mock-mirror-444555" },
    ],
    version: null,
    userAgent: null,
    webSearch: null,
    extra: {},
    enabled: true,
    createdAt: now - 30 * 24 * HOUR,
    updatedAt: now - 2 * HOUR,
  },
  {
    key: "openai-relay",
    endpoints: [
      { protocol: "openai-chat", baseUrl: "https://relay.example.com/v1", apiKey: "sk-mock-relay-abc123" },
    ],
    version: null,
    userAgent: null,
    webSearch: null,
    extra: {},
    enabled: true,
    createdAt: now - 20 * 24 * HOUR,
    updatedAt: now - 26 * HOUR,
  },
  {
    key: "gemini-direct",
    endpoints: [
      { protocol: "google-genai", baseUrl: "https://generativelanguage.googleapis.com", apiKey: "AIza-mock-gemini-xyz" },
    ],
    version: "v1beta",
    userAgent: null,
    webSearch: null,
    extra: {},
    enabled: false,
    createdAt: now - 10 * 24 * HOUR,
    updatedAt: now - 5 * 24 * HOUR,
  },
];

const seedModels: ModelDef[] = [
  {
    slug: "claude-sonnet-4",
    displayName: "Claude Sonnet 4",
    contextWindow: 200_000,
    modalities: ["text"],
    reasoningLevels: ["standard", "extended"],
    pricing: { input: 3, output: 15, currency: "USD/1M" },
    extra: {},
  },
  {
    slug: "deepseek-v4-pro",
    displayName: "DeepSeek V4 Pro",
    contextWindow: 128_000,
    modalities: ["text"],
    reasoningLevels: ["standard"],
    pricing: { input: 0.27, output: 1.1, currency: "USD/1M" },
    extra: {},
  },
  {
    slug: "deepseek-v4-flash",
    displayName: "DeepSeek V4 Flash",
    contextWindow: 128_000,
    modalities: ["text"],
    reasoningLevels: ["standard"],
    pricing: { input: 0.07, output: 0.28, currency: "USD/1M" },
    extra: {},
  },
];

const seedOffers: Offer[] = [
  { providerKey: "anthropic-official", modelSlug: "claude-sonnet-4", pricing: { input: 3, output: 15 } },
  { providerKey: "openai-relay", modelSlug: "deepseek-v4-pro", pricing: { input: 0.3, output: 1.2 } },
  { providerKey: "openai-relay", modelSlug: "deepseek-v4-flash", pricing: { input: 0.08, output: 0.3 } },
  { providerKey: "gemini-direct", modelSlug: "deepseek-v4-pro", pricing: null },
];

const seedRoutes: Route[] = [
  { alias: "claude-sonnet-4", modelSlug: "claude-sonnet-4", providerKey: "anthropic-official", extra: {} },
  { alias: "deepseek-pro", modelSlug: "deepseek-v4-pro", providerKey: "openai-relay", extra: {} },
];

const DEFAULT_SCRIPT = `-- Moon Bridge Next 插件示例：为出站请求注入自定义头
-- 可用钩子：on_request（入站 CoreRequest → 出站前）、on_response
function on_request(ctx, req)
  req.headers["x-mock-inject"] = "hello"
  return req
end
`;

const seedPlugins: PluginRecord[] = [
  {
    name: "header-inject",
    source: "builtin",
    scriptRef: "plugins/header-inject.lua",
    enabled: true,
    config: { header: "x-mock-inject" },
    scopes: ["request"],
    capabilities: ["http"],
  },
  {
    name: "redact-sensitive",
    source: "builtin",
    scriptRef: "plugins/redact-sensitive.lua",
    enabled: false,
    config: { keys: ["authorization", "api-key"] },
    scopes: ["response"],
    capabilities: [],
  },
];

const seedBindings: PluginBinding[] = [
  { pluginName: "header-inject", scope: "provider", scopeKey: "anthropic-official", enabled: true, config: {} },
];

// 生成最近 48h 的用量记录：每小时 0–3 条，模型加权随机，少量 error。
// 注意：createdAt 与后端契约一致使用 unix 秒（store::now_unix）。
function seedUsage(): UsageRecord[] {
  const models = ["claude-sonnet-4", "deepseek-v4-pro", "deepseek-v4-flash"];
  const weights = [0.5, 0.3, 0.2];
  const recs: UsageRecord[] = [];
  let seq = 0;
  const pick = () => {
    const r = Math.random();
    let acc = 0;
    for (let i = 0; i < models.length; i++) {
      acc += weights[i]!;
      if (r <= acc) return models[i]!;
    }
    return models[0]!;
  };
  const start = Math.floor((now - 48 * HOUR) / 1000);
  for (let h = 0; h < 48; h++) {
    const n = Math.floor(Math.random() * 4);
    for (let i = 0; i < n; i++) {
      const isErr = Math.random() < 0.08;
      const input = 800 + Math.floor(Math.random() * 18_000);
      const output = isErr ? 0 : 120 + Math.floor(Math.random() * 6_000);
      const cacheRead = Math.random() < 0.4 ? Math.floor(Math.random() * 8_000) : 0;
      const cacheWrite = Math.random() < 0.2 ? Math.floor(Math.random() * 2_000) : 0;
      const reasoning = isErr || Math.random() < 0.4 ? 0 : Math.floor(Math.random() * 1_500);
      const model = pick();
      const latencyMs = isErr ? 200 + Math.random() * 400 : 400 + Math.random() * 3_600;
      // 首字延迟：仅成功流式请求有值，且必小于总延迟
      const ttftMs = isErr ? null : Math.floor(100 + Math.random() * (latencyMs - 200));
      recs.push({
        id: `u-${(seq++).toString(36).padStart(4, "0")}`,
        sessionId: `sess-${Math.random().toString(36).slice(2, 10)}`,
        model,
        upstreamModel: model,
        inputTokens: input,
        outputTokens: output,
        cacheReadTokens: cacheRead,
        cacheWriteTokens: cacheWrite,
        reasoningTokens: reasoning,
        cost: Number(((input * 3 + output * 15) / 1_000_000).toFixed(6)),
        status: isErr ? "error" : "ok",
        error: isErr ? "upstream 502: bad gateway (mock)" : null,
        latencyMs,
        ttftMs,
        createdAt: start + h * 3600 + Math.floor(Math.random() * 3600),
      });
    }
  }
  return recs;
}

// ───────────────────────── 内存态 ─────────────────────────

const providers = [...seedProviders];
const models = [...seedModels];
const offers = [...seedOffers];
const routes = [...seedRoutes];
const plugins = [...seedPlugins];
const bindings = [...seedBindings];
const settings = new Map<string, Json>([
  ["ui.theme", "system"],
  ["gateway.defaultAlias", "claude-sonnet-4"],
]);
const usageRecords = seedUsage();
const scripts = new Map<string, string>(seedPlugins.map((p) => [p.name, DEFAULT_SCRIPT]));

const gatewayStatus: GatewayStatus = { running: false, addr: "127.0.0.1:8787", error: null };

const appConfig: AppConfig = {
  gateway: {
    addr: "127.0.0.1:8787",
    authToken: null,
    egressProxy: null,
    maxBodyBytes: 10_485_760,
    requestTimeoutSecs: 120,
    traceDir: null,
  } satisfies GatewayConfig,
  logLevel: "info",
  autoStart: true,
};

const appInfo: AppInfo = {
  version: "0.1.0",
  dbPath: "~/.local/share/moon-bridge-next/moonbridge.db (mock)",
  configPath: "~/.config/moon-bridge-next/config.json (mock)",
  dataDir: "~/.local/share/moon-bridge-next (mock)",
  pluginsDir: "~/.local/share/moon-bridge-next/plugins (mock)",
  traceDir: "~/.local/share/moon-bridge-next/traces (mock)",
};

// trace：取最近 8 条成功用量记录构造对应快照。
const traceEntries: TraceEntry[] = [];
const traceDetails = new Map<string, TraceDetail>();
{
  const okRecs = usageRecords.filter((r) => r.status === "ok").slice(-8).reverse();
  for (const r of okRecs) {
    const requestId = `req-${r.id}`;
    const fileName = `${r.createdAt}-${r.id}.json`;
    const relPath = `${r.sessionId}/${r.model}/${fileName}`;
    const provider = providers.find((p) => p.enabled) ?? providers[0]!;
    traceEntries.push({
      session: r.sessionId ?? "unknown",
      model: r.model ?? "unknown",
      fileName,
      relPath,
      modifiedAt: r.createdAt * 1000,
      size: 1_800 + Math.floor(Math.random() * 2_400),
    });
    traceDetails.set(relPath, {
      requestId,
      createdAt: r.createdAt * 1000,
      sessionId: r.sessionId,
      modelAlias: r.model ?? r.upstreamModel ?? "unknown",
      upstreamModel: r.upstreamModel ?? r.model ?? "unknown",
      providerKey: provider.key,
      clientProtocol: "anthropic",
      upstreamProtocol: provider.endpoints[0]?.protocol,
      stream: true,
      status: "ok",
      latencyMs: r.latencyMs,
      usage: {
        inputTokens: r.inputTokens,
        outputTokens: r.outputTokens,
        cacheReadTokens: r.cacheReadTokens,
        cacheWriteTokens: r.cacheWriteTokens,
        reasoningTokens: r.reasoningTokens,
      },
      clientRequest: {
        model: r.model,
        max_tokens: 1024,
        messages: [{ role: "user", content: "用一句话解释什么是 LLM 网关。(mock)" }],
      },
      upstreamRequest: {
        model: r.upstreamModel,
        messages: [{ role: "user", content: "用一句话解释什么是 LLM 网关。(mock)" }],
      },
      upstreamResponse: {
        choices: [{ message: { role: "assistant", content: "LLM 网关是位于应用与大模型之间的协议转换与治理层。(mock)" } }],
      },
      clientResponse: {
        content: [{ type: "text", text: "LLM 网关是位于应用与大模型之间的协议转换与治理层。(mock)" }],
        stop_reason: "end_turn",
      },
      error: null,
    });
  }
}

// ───────────────────────── CRUD helper ─────────────────────────

function upsert<T>(list: T[], item: T, keyOf: (x: T) => string): void {
  const key = keyOf(item);
  const idx = list.findIndex((x) => keyOf(x) === key);
  if (idx >= 0) list[idx] = item;
  else list.push(item);
}

// ───────────────────────── command 分发 ─────────────────────────

/** 按 command 名分发到内存实现；与真实 invoke 的参数/返回契约一致。 */
export async function mockInvoke<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  await delay();

  switch (cmd) {
    // ── 网关生命周期 ──
    case "gateway_start":
    case "gateway_restart":
      gatewayStatus.running = true;
      gatewayStatus.error = null;
      return { ...gatewayStatus } as T;
    case "gateway_stop":
      gatewayStatus.running = false;
      return { ...gatewayStatus } as T;
    case "gateway_status":
      return { ...gatewayStatus } as T;

    // ── Provider ──
    case "provider_list":
      return providers.map((p) => ({ ...p })) as T;
    case "provider_get":
      return (providers.find((p) => p.key === args.key) ?? null) as T;
    case "provider_save": {
      const p = (args as { provider: Provider }).provider;
      p.updatedAt = Date.now();
      upsert(providers, { ...p }, (x) => x.key);
      return undefined as T;
    }
    case "provider_delete": {
      const idx = providers.findIndex((p) => p.key === args.key);
      if (idx < 0) throw new Error(`provider 不存在：${String(args.key)}`);
      providers.splice(idx, 1);
      return undefined as T;
    }

    // ── Model & Offer ──
    case "model_list":
      return models.map((m) => ({ ...m })) as T;
    case "model_get":
      return (models.find((m) => m.slug === args.slug) ?? null) as T;
    case "model_save":
      upsert(models, { ...(args as { model: ModelDef }).model }, (x) => x.slug);
      return undefined as T;
    case "model_delete": {
      const idx = models.findIndex((m) => m.slug === args.slug);
      if (idx < 0) throw new Error(`模型不存在：${String(args.slug)}`);
      models.splice(idx, 1);
      return undefined as T;
    }
    case "offer_list":
      return offers.filter((o) => o.providerKey === args.providerKey).map((o) => ({ ...o })) as T;
    case "offer_save":
      upsert(
        offers,
        { ...(args as { offer: Offer }).offer },
        (x) => `${x.providerKey}::${x.modelSlug}`,
      );
      return undefined as T;
    case "offer_delete": {
      const idx = offers.findIndex(
        (o) => o.providerKey === args.providerKey && o.modelSlug === args.modelSlug,
      );
      if (idx >= 0) offers.splice(idx, 1);
      return undefined as T;
    }

    // ── Route ──
    case "route_list":
      return routes.map((r) => ({ ...r })) as T;
    case "route_get":
      return (routes.find((r) => r.alias === args.alias) ?? null) as T;
    case "route_save":
      upsert(routes, { ...(args as { route: Route }).route }, (x) => x.alias);
      return undefined as T;
    case "route_delete": {
      const idx = routes.findIndex((r) => r.alias === args.alias);
      if (idx < 0) throw new Error(`路由不存在：${String(args.alias)}`);
      routes.splice(idx, 1);
      return undefined as T;
    }

    // ── Plugin & Binding ──
    case "plugin_list":
      return plugins.map((p) => ({ ...p })) as T;
    case "plugin_get":
      return (plugins.find((p) => p.name === args.name) ?? null) as T;
    case "plugin_save":
      upsert(plugins, { ...(args as { plugin: PluginRecord }).plugin }, (x) => x.name);
      return undefined as T;
    case "plugin_delete": {
      const idx = plugins.findIndex((p) => p.name === args.name);
      if (idx < 0) throw new Error(`插件不存在：${String(args.name)}`);
      plugins.splice(idx, 1);
      scripts.delete(String(args.name));
      return undefined as T;
    }
    case "plugin_read_script": {
      const s = scripts.get(String(args.name));
      if (s === undefined) throw new Error(`脚本不存在：${String(args.name)}`);
      return s as T;
    }
    case "plugin_write_script":
      scripts.set(String(args.name), String(args.content ?? ""));
      return undefined as T;
    case "binding_list":
      return bindings.filter((b) => b.pluginName === args.pluginName).map((b) => ({ ...b })) as T;
    case "binding_list_by_scope":
      return bindings.filter((b) => b.scope === args.scope).map((b) => ({ ...b })) as T;
    case "binding_save":
      upsert(
        bindings,
        { ...(args as { binding: PluginBinding }).binding },
        (x) => `${x.pluginName}::${x.scope}::${x.scopeKey}`,
      );
      return undefined as T;
    case "binding_delete": {
      const idx = bindings.findIndex(
        (b) =>
          b.pluginName === args.pluginName && b.scope === args.scope && b.scopeKey === args.scopeKey,
      );
      if (idx >= 0) bindings.splice(idx, 1);
      return undefined as T;
    }

    // ── Usage ──
    case "usage_query": {
      let list = usageRecords;
      if (args.model) list = list.filter((r) => r.model === args.model);
      if (args.status) list = list.filter((r) => r.status === args.status);
      const offset = Number(args.offset ?? 0);
      const limit = Number(args.limit ?? 500);
      return list.slice(offset, offset + limit).map((r) => ({ ...r })) as T;
    }
    case "usage_summary": {
      const s: UsageSummary = { requests: 0, inputTokens: 0, outputTokens: 0, totalCost: 0 };
      for (const r of usageRecords) {
        s.requests++;
        s.inputTokens += r.inputTokens;
        s.outputTokens += r.outputTokens;
        s.totalCost += r.cost;
      }
      s.totalCost = Number(s.totalCost.toFixed(6));
      return s as T;
    }

    // ── Trace ──
    case "trace_list": {
      const limit = Number(args.limit ?? 500);
      return [...traceEntries]
        .sort((a, b) => b.modifiedAt - a.modifiedAt)
        .slice(0, limit)
        .map((t) => ({ ...t })) as T;
    }
    case "trace_read": {
      const d = traceDetails.get(String(args.relPath));
      if (!d) throw new Error(`trace 不存在：${String(args.relPath)}`);
      return d as T;
    }
    case "trace_delete": {
      const rel = String(args.relPath ?? "");
      const idx = traceEntries.findIndex((t) => t.relPath === rel);
      if (idx < 0) throw new Error(`trace 不存在：${rel}`);
      traceEntries.splice(idx, 1);
      traceDetails.delete(rel);
      return undefined as T;
    }

    // ── Settings ──
    case "settings_get": {
      if (!settings.has(String(args.key))) throw new Error(`设置不存在：${String(args.key)}`);
      return settings.get(String(args.key)) as T;
    }
    case "settings_set":
      settings.set(String(args.key), args.value as Json);
      return undefined as T;
    case "settings_list":
      return [...settings.entries()].map(([key, value]) => ({ key, value })) as T;
    case "settings_delete":
      settings.delete(String(args.key));
      return undefined as T;

    // ── App ──
    case "app_info":
      return { ...appInfo } as T;
    case "config_get":
      return structuredClone(appConfig) as T;
    case "config_set":
      Object.assign(appConfig, args.config);
      return undefined as T;

    default:
      throw new Error(`mock: 未实现的 command：${cmd}`);
  }
}
