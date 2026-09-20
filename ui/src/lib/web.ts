// web 模式传输层：把 Tauri command 映射到同源 /api/* HTTP 接口（fetch + Bearer token）。
// 仅在非 Tauri 且未强制 mock（见 api.ts 的 isWebRuntime）时启用；桌面端与 mock 不走这里。

import type { PluginImportOutcome } from "./api";

let adminToken = "";

try {
  localStorage.removeItem("mb.adminToken");
} catch {
  // 禁用存储时仍可使用仅内存中的令牌。
}

/** 管理令牌仅保存在当前页面内存中，刷新页面后需重新登录。 */
export function getToken(): string {
  return adminToken;
}

export function setToken(token: string) {
  adminToken = token.trim();
}

export function clearToken() {
  adminToken = "";
}

/** 认证失效事件：收到 401 时派发，App.vue 监听后弹出登录卡片。 */
export const UNAUTHORIZED_EVENT = "mb:unauthorized";

/** 带状态码的 HTTP 错误：供调用方区分 401 / 404 等语义。 */
class HttpError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

type Method = "GET" | "POST" | "PUT" | "DELETE";

interface RequestOptions {
  /** 请求体；默认按 JSON 序列化。 */
  body?: unknown;
  /** 以 text/plain 原文收发（插件脚本端点）。 */
  text?: boolean;
}

/** 从错误响应体中提取可读消息（后端统一 `{ message }`）。 */
async function readMessage(res: Response): Promise<string> {
  try {
    const raw = await res.text();
    if (raw) {
      const data = JSON.parse(raw) as { message?: unknown };
      if (data && typeof data.message === "string") return data.message;
      return raw;
    }
  } catch {
    // 非 JSON 错误体：回落状态码文本
  }
  return `HTTP ${res.status} ${res.statusText}`;
}

async function request<T>(method: Method, path: string, opts: RequestOptions = {}): Promise<T> {
  const headers: Record<string, string> = {};
  const token = getToken();
  if (token) headers.Authorization = `Bearer ${token}`;

  let body: string | undefined;
  if (opts.body !== undefined) {
    if (opts.text) {
      headers["Content-Type"] = "text/plain;charset=UTF-8";
      body = String(opts.body);
    } else {
      headers["Content-Type"] = "application/json";
      body = JSON.stringify(opts.body);
    }
  }

  const res = await fetch(path, { method, headers, body });
  if (!res.ok) {
    if (res.status === 401) {
      clearToken();
      window.dispatchEvent(new Event(UNAUTHORIZED_EVENT));
    }
    throw new HttpError(res.status, await readMessage(res));
  }

  if (opts.text) return (await res.text()) as T;
  const raw = await res.text();
  return (raw ? JSON.parse(raw) : null) as T;
}

/** 无返回值操作（REST 返回 null / 空体）的占位返回值。 */
function ok<T>(): T {
  return null as unknown as T;
}

/** 对象不存在时后端返回 404，对齐 Tauri 侧的 `Option → null` 语义。 */
async function orNull<T>(p: Promise<T>): Promise<T | null> {
  try {
    return await p;
  } catch (e) {
    if (e instanceof HttpError && e.status === 404) return null;
    throw e;
  }
}

/** URL 路径段编码（scopeKey 等可能含特殊字符）。 */
function seg(v: unknown): string {
  return encodeURIComponent(String(v));
}

/** 拼接 camelCase 查询串，跳过未提供（undefined / null / 空串）的参数。 */
function query(params: Record<string, unknown>): string {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v === undefined || v === null || v === "") continue;
    q.set(k, String(v));
  }
  const s = q.toString();
  return s ? `?${s}` : "";
}

/** command 名 → REST 路由的分发（路由表见 docs/architecture.md 管理 API 一节）。 */
export async function webInvoke<T>(cmd: string, args: Record<string, unknown>): Promise<T> {
  switch (cmd) {
    // ── 网关：web 模式下进程由外部托管，只支持重启 ──
    case "gateway_start":
    case "gateway_stop":
      throw new Error("web 模式不支持启停网关，请使用重启");
    case "gateway_restart":
      // 202 后在原进程内排空请求并重建监听器；状态由调用方轮询刷新。
      await request("POST", "/api/gateway/restart");
      return ok<T>();
    case "gateway_status":
      return (await request<unknown>("GET", "/api/gateway/status")) as T;

    // ── 上游服务 ──
    case "provider_list":
      return (await request<unknown>("GET", "/api/providers")) as T;
    case "provider_get":
      return (await orNull(request<unknown>("GET", `/api/providers/${seg(args.key)}`))) as T;
    case "provider_save":
      await request("PUT", "/api/providers", { body: args.provider });
      return ok<T>();
    case "provider_delete":
      await request("DELETE", `/api/providers/${seg(args.key)}`);
      return ok<T>();

    // ── 模型与报价 ──
    case "model_list":
      return (await request<unknown>("GET", "/api/models")) as T;
    case "model_get":
      return (await orNull(request<unknown>("GET", `/api/models/${seg(args.slug)}`))) as T;
    case "model_save":
      await request("PUT", "/api/models", { body: args.model });
      return ok<T>();
    case "model_delete":
      await request("DELETE", `/api/models/${seg(args.slug)}`);
      return ok<T>();
    case "offer_list":
      return (await request<unknown>("GET", `/api/providers/${seg(args.providerKey)}/offers`)) as T;
    case "offer_save":
      await request("PUT", "/api/offers", { body: args.offer });
      return ok<T>();
    case "offer_delete":
      await request("DELETE", `/api/providers/${seg(args.providerKey)}/offers/${seg(args.modelSlug)}`);
      return ok<T>();

    // ── 模型目录（models.dev）──
    case "catalog_fetch":
      return (await request<unknown>("GET", "/api/catalog")) as T;
    case "catalog_import": {
      const res = await request<{ imported: number }>("POST", "/api/catalog/import", {
        body: args.models,
      });
      return (res?.imported ?? 0) as T;
    }

    // ── 路由 ──
    case "route_list":
      return (await request<unknown>("GET", "/api/routes")) as T;
    case "route_get":
      return (await orNull(request<unknown>("GET", `/api/routes/${seg(args.alias)}`))) as T;
    case "route_save":
      await request("PUT", "/api/routes", { body: args.route });
      return ok<T>();
    case "route_delete":
      await request("DELETE", `/api/routes/${seg(args.alias)}`);
      return ok<T>();

    // ── 插件与绑定 ──
    case "plugin_list":
      return (await request<unknown>("GET", "/api/plugins")) as T;
    case "plugin_get":
      return (await orNull(request<unknown>("GET", `/api/plugins/${seg(args.name)}`))) as T;
    case "plugin_save":
      await request("PUT", "/api/plugins", { body: args.plugin });
      return ok<T>();
    case "plugin_delete":
      await request("DELETE", `/api/plugins/${seg(args.name)}`);
      return ok<T>();
    // web 模式无磁盘路径，导入改由 importPluginFiles 上传内容
    case "plugin_import":
      throw new Error("web 模式请使用文件选择器导入插件");
    case "plugin_read_script":
      return (await request<unknown>("GET", `/api/plugins/${seg(args.name)}/script`, { text: true })) as T;
    case "plugin_write_script":
      await request("PUT", `/api/plugins/${seg(args.name)}/script`, {
        body: args.content,
        text: true,
      });
      return ok<T>();
    case "binding_list":
      return (await request<unknown>("GET", `/api/plugins/${seg(args.pluginName)}/bindings`)) as T;
    case "binding_list_by_scope":
      return (await request<unknown>("GET", `/api/bindings${query({ scope: args.scope })}`)) as T;
    case "binding_save":
      await request("PUT", "/api/bindings", { body: args.binding });
      return ok<T>();
    case "binding_delete":
      await request(
        "DELETE",
        `/api/bindings/${seg(args.pluginName)}/${seg(args.scope)}/${seg(args.scopeKey)}`,
      );
      return ok<T>();

    // ── 余额看板 ──
    case "balance_card_list":
      return (await request<unknown>("GET", "/api/balance/cards")) as T;
    case "balance_card_save":
      await request("PUT", "/api/balance/cards", { body: args.card });
      return ok<T>();
    case "balance_card_delete":
      await request("DELETE", `/api/balance/cards/${seg(args.key)}`);
      return ok<T>();
    case "balance_card_refresh": {
      const qs =
        args.keyIndex !== null && args.keyIndex !== undefined
          ? `?key_index=${encodeURIComponent(String(args.keyIndex))}`
          : "";
      return (await request<unknown>("POST", `/api/balance/cards/${seg(args.key)}/refresh${qs}`)) as T;
    }
    case "balance_refresh_all":
      return (await request<unknown>("POST", "/api/balance/refresh")) as T;
    case "balance_card_test":
      return (await request<unknown>("POST", "/api/balance/test", { body: args.card })) as T;

    // ── 用量与链路追踪 ──
    case "usage_query":
      return (await request<unknown>(
        "GET",
        `/api/usage${query({
          model: args.model,
          providerKey: args.providerKey,
          status: args.status,
          since: args.since,
          until: args.until,
          limit: args.limit,
          offset: args.offset,
        })}`,
      )) as T;
    case "usage_summary":
      return (await request<unknown>(
        "GET",
        `/api/usage/summary${query({ since: args.since, until: args.until })}`,
      )) as T;
    case "trace_list":
      return (await request<unknown>("GET", `/api/traces${query({ limit: args.limit })}`)) as T;
    case "trace_read":
      return (await request<unknown>("GET", `/api/trace${query({ path: args.relPath })}`)) as T;
    case "trace_delete":
      await request("DELETE", `/api/trace${query({ path: args.relPath })}`);
      return ok<T>();

    // ── 设置与应用信息 ──
    case "settings_get":
      return (await orNull(request<unknown>("GET", `/api/settings/${seg(args.key)}`))) as T;
    case "settings_set":
      // body 为 JSON 值原文（后端按任意 JSON 解析）
      await request("PUT", `/api/settings/${seg(args.key)}`, { body: args.value });
      return ok<T>();
    case "settings_list":
      return (await request<unknown>("GET", "/api/settings")) as T;
    case "settings_delete":
      await request("DELETE", `/api/settings/${seg(args.key)}`);
      return ok<T>();
    case "app_info":
      return (await request<unknown>("GET", "/api/app/info")) as T;
    case "config_get":
      return (await request<unknown>("GET", "/api/config")) as T;
    case "config_set":
      await request("PUT", "/api/config", { body: args.config });
      return ok<T>();

    default:
      throw new Error(`web 模式未实现该 command：${cmd}`);
  }
}

/** web 模式插件导入：浏览器无磁盘路径，改上传文件内容（响应形状与 Tauri 侧一致）。 */
export async function importPluginFiles(
  files: { name: string; content: string }[],
): Promise<PluginImportOutcome[]> {
  const res = await request<PluginImportOutcome[]>("POST", "/api/plugins/import", {
    body: { files },
  });
  return Array.isArray(res) ? res : [];
}
