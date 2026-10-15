<div align="center">

# Moon Bridge Next

### Local LLM gateway: protocol translation, Lua plugins, usage & quota dashboards

[![License](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey.svg)]()
[![Built with Tauri](https://img.shields.io/badge/built%20with-Tauri%202-orange.svg)](https://tauri.app/)
[![Backend](https://img.shields.io/badge/backend-Rust-dea584.svg)](https://www.rust-lang.org/)
[![Frontend](https://img.shields.io/badge/frontend-Vue%203-42b883.svg)](https://vuejs.org/)

[中文](README.md) · English · [Architecture](docs/architecture.md) · [Development](DEVELOPMENT.md)

</div>

## Overview

LLM clients and upstream providers speak different API protocols: Claude Code uses Anthropic Messages, Codex uses OpenAI Responses, and most tools use Chat Completions. Switching upstream providers usually requires reconfiguring clients or switching tools.

Moon Bridge Next sits between your clients and upstream providers, translating requests into the target upstream protocol. It includes a Lua 5.4 plugin system that can modify the normalized intermediate representation (Core IR) or rewrite raw HTTP messages and SSE chunks. Everything is managed through a built-in web/desktop UI.

- **Four-protocol translation**: Bidirectional streaming and non-streaming translation across OpenAI Responses, OpenAI Chat Completions, Anthropic Messages, and Google Gemini.
- **Two-layer Lua plugins**: Semantic hooks (`core`) operate on Core IR and apply across all protocols; wire hooks (`raw_*`) read and write HTTP headers, bodies, and SSE chunks directly.
- **Usage metering & quota monitoring**: Tracks five token categories, latency, and cost per request. Includes 12 built-in quota adapters to monitor upstream balances and rate limits per API key.
- **Four-stage request tracing**: Snapshots raw payloads at inbound request, outbound request, upstream response, and client response stages for debugging protocol translation and plugin rewrites.
- **Desktop app & headless server**: Ships as a Tauri desktop app and as `moonbridge-server`, which serves the gateway, management REST API, and web UI on a single port.
- **Encrypted credentials at rest**: Provider API keys and quota configurations are encrypted with AES-GCM, with support for entry bearer tokens, private-origin allowlists, and egress proxies.

## Screenshots

|                     Dashboard                     |                    Usage                     |
| :-----------------------------------------------: | :------------------------------------------: |
| ![Dashboard](docs/screenshots/dashboard.png)      |  ![Usage](docs/screenshots/usage.png)        |
|                  **Request Traces**               |              **Quota Dashboard**             |
|   ![Traces](docs/screenshots/traces.png)          |  ![Quota](docs/screenshots/quota.png)        |
|                  **Plugin Editor**                |               **Model Catalog**              |
| ![Plugin Editor](docs/screenshots/plugin-editor.png) | ![Models](docs/screenshots/models.png)   |

## Protocol Support

Protocol translation is centered on Core IR (`CoreRequest` / `CoreResponse` / `CoreStreamEvent`). Inbound requests are normalized into Core IR and then encoded into the target protocol by the upstream adapter, reducing N×M pairwise converters to N+M.

| Protocol | Entry (client → gateway) | Upstream (gateway → provider) |
|------|:---:|:---:|
| OpenAI Responses (`/v1/responses`) | ✓ | ✓ |
| OpenAI Chat Completions (`/v1/chat/completions`) | ✓ | ✓ |
| Anthropic Messages (`/v1/messages`) | ✓ | ✓ |
| Google Generative AI / Gemini | adapter implemented, route unmounted | ✓ |

All four adapters support both non-streaming and streaming (SSE). Core IR unifies content block types (text, image, document, tool use, tool result, reasoning) and token accounting (`input_tokens` includes cache reads/writes; `output_tokens` includes reasoning tokens).

Auxiliary endpoints: `GET /health` and `GET /v1/models`.

## Features

### Provider and Model Management

- **Multi-endpoint failover**: A provider can configure multiple endpoints, each with its own protocol, base URL, and API key. Requests fail over in endpoint order.
- **Model catalog**: Search and batch-import models from models.dev (including context window, max output tokens, modalities, reasoning levels, and pricing), or define models manually.
- **Offers and pricing**: Models attach to providers via offers. Each offer specifies per-1M-token rates (input, output, cache read, cache write, reasoning, plus long-context tiers) and an optional endpoint protocol constraint.
- **Route aliases**: Clients request models by alias, which resolves to a provider and upstream model.

### Usage Metering

Each request records the session ID, client model alias, upstream model, provider, five token counters (input, output, cache read, cache write, reasoning), time to first token (TTFT), total latency, status (`ok` / `error` / `aborted`), and cost. Cost is calculated at record time from the matched offer's pricing; subsequent price edits do not alter historical records. The Usage page provides summary cards, stacked token time series, model breakdown, and per-request logs.

### Request Tracing

When tracing is enabled, raw messages are saved to disk at four stages: inbound request, outbound request, upstream response, and client response. For streaming requests, the gateway reassembles the event stream into a complete response object before saving, making it easy to compare what the upstream sent against what the client received. The Traces page supports filtering, side-by-side inspection, and deletion.

### Quota Dashboard

The Quota page groups providers and displays per-key quota and health status. When a provider is bound to a `quota` plugin, the engine runs `MB.query` once per endpoint API key and renders a row per key with a masked key label, status badge, and progress bars.

Built-in quota plugins cover new-api, Sub2API, DeepSeek, Moonshot/Kimi, SiliconFlow, OpenRouter, Zhipu GLM Coding Plan, Kimi Coding Plan, CommandCode, and Claude Code. Custom `quota` plugins can be written in Lua using `mb.http.request` to return percentage, amount, or counter quota entries. The provider editor supports configurable polling intervals, encrypted plugin parameters, and a dry-run preview before saving.

### Session Recognition

Session identity is resolved in the following order to isolate `mb.session` plugin state and organize trace directories:

1. Inbound wire-hook override on `msg.session_id` (built-in harness presets in `plugins/harness/` extract session headers or metadata for Claude Code, Codex, OpenCode, Kimi Code, Grok Build, and DSH).
2. Explicit request identifiers: `session_id` or `previous_response_id` in the request body, or the `X-Codex-Window-Id` header.
3. Session marker: For clients that send no session identifier, the gateway embeds an `[mb:<payload>]` marker at the start of the first non-redacted reasoning (CoT) block. When the client sends the conversation history back on the next turn, the gateway extracts the session UUID and strips the marker before forwarding the request upstream. This can be disabled in Settings (inbound markers are still stripped when disabled).

### Access Control

In desktop mode, an optional `auth_token` requires `Authorization: Bearer <token>` on client POST requests; when unset, no entry authentication is enforced (suitable for loopback listening). In headless server mode, separate `--admin-token` and `--gateway-token` values are required. `GET /health` and `GET /v1/models` remain public.

## Quick Start

1. Launch the app; the gateway listens on `127.0.0.1:38440` by default (configurable in Settings).
2. On the **Providers** page, add an upstream provider with its protocol, base URL, and API key (add multiple endpoints for failover if needed).
3. On the **Models** page, import models from models.dev or create them manually, then create offers for your provider.
4. On the **Routes** page, create an alias pointing to the provider and model.
5. Point your client at the gateway using the route alias as the model name:
   - OpenAI-compatible clients (Chat / Responses): base URL `http://127.0.0.1:38440/v1`
   - Anthropic clients: base URL `http://127.0.0.1:38440`
   - API key: the gateway's `auth_token` if configured, or any non-empty placeholder string
6. Configure plugins on the **Plugins** page as needed, then click "Restart gateway" to apply.

See [DEVELOPMENT.md](DEVELOPMENT.md) for building from source.

## Plugin System

Plugins are Lua 5.4 scripts running in isolated sandboxes with two hook layers:

- **Semantic layer (`core`)**: Runs after protocol normalization on the unified Core IR, applying across all protocols.
- **Wire layer (`raw_request` / `raw_response` / `raw_stream`)**: Runs at the outer edge of protocol conversion, reading and writing raw HTTP headers, bodies, and SSE chunks.

### Common Use Cases

Semantic layer (`core`):
- Inject or rewrite system prompts, append messages, or adjust `temperature`, `max_tokens`, and `reasoning` parameters
- Append tool definitions dynamically via `inject_tools`
- Filter content blocks via `filter_content` (e.g., strip reasoning blocks) or drop streaming events via `on_stream_event`
- Rewrite error messages, log request metadata, or track per-session state via `mb.session`

Wire layer (`raw_*`):
- Add, remove, or modify HTTP headers, inject vendor-specific headers, override `User-Agent`, or sign requests with `mb.crypto`
- Patch outbound JSON bodies, enforce custom entry authorization (`abort`), or serve cached responses locally (`short_circuit`)
- Retry transient upstream failures by returning `retry` from the error path of `on_upstream_response_raw` (see bundled `plugins/utils/rescue_5xx.lua` and `rescue_fake_200.lua`)
- Drop heartbeat SSE chunks or orchestrate sub-requests to other providers via `mb.provider.invoke`

### Plugin Manifest (`MB`)

A plugin script defines a global `MB` table containing manifest fields and hook functions:

| Field | Description |
|------|------|
| `name` | Unique plugin name (the name registered in the UI is authoritative) |
| `version` | Version string |
| `category` | Plugin category: `core` (default, request pipeline) or `quota` (quota polling) |
| `scopes` | Allowed attachment scopes: `global` / `provider` / `model` / `route` |
| `capabilities` | Enabled hook layers: `core` / `raw_request` / `raw_response` / `raw_stream` |
| `config_schema` | Optional JSON Schema used by the UI to render the configuration form |
| `requires` | Optional gateway setting prerequisites (e.g., `{ sessionMarker = true }`) |
| `init` / `shutdown` | Optional lifecycle hooks called before serving starts and after draining completes |

### Semantic-Layer Hooks

| Hook | Description |
|------|------|
| `on_request(ctx, req)` | Modify `CoreRequest` in place or return a replacement table |
| `inject_tools(ctx)` | Return an array of `Tool` definitions to append |
| `on_response(ctx, resp)` | Modify non-streaming `CoreResponse` |
| `on_stream_event(ctx, ev)` | Inspect or modify a streaming event; return `true` to drop it |
| `filter_content(ctx, block)` | Return `true` to skip a content block (also suppresses its subsequent stream deltas) |
| `transform_error(ctx, msg)` | Return a rewritten error message string |

`CoreRequest` fields: `model` (resolved upstream model), `model_alias`, `system` (array of `ContentBlock`), `messages`, `tools`, `tool_choice`, `max_tokens`, `temperature`, `top_p`, `stop`, `stream`, `reasoning`, `meta`.
`CoreResponse` fields: `id`, `model`, `content`, `stop_reason`, `usage` (`input_tokens` / `output_tokens` / `cache_read_tokens` / `cache_write_tokens` / `reasoning_tokens`).

### Wire-Layer Hooks

| Hook | Capability | Trigger |
|------|-----------|---------|
| `on_client_request_raw(ctx, msg)` | `raw_request` | Inbound request (client → gateway) |
| `on_upstream_request_raw(ctx, msg)` | `raw_request` | Outbound request (gateway → upstream) |
| `on_upstream_response_raw(ctx, msg)` | `raw_response` | Upstream response (including non-2xx responses and zero-output stream failures, where `msg.error = true`) |
| `on_client_response_raw(ctx, msg)` | `raw_response` | Outbound response (gateway → client, non-streaming) |
| `on_upstream_chunk_raw(ctx, chunk)` | `raw_stream` | Inbound SSE chunk from upstream |
| `on_client_chunk_raw(ctx, chunk)` | `raw_stream` | Outbound SSE chunk to client |

- `msg` fields: `stage`, `protocol`, `provider`, `method`, `url`, `status`, `headers` (order-preserving array `{{k, v}, ...}`), `body` (JSON table / string / nil; `nil` with `body_truncated = true` when exceeding the size limit), `session_id` (writable in `client_request` stage to override session identity), and `error` (read-only boolean indicating the upstream error path). For outbound requests, in-place edits to `method`, `url`, `headers`, and `body` are applied.
- `chunk` fields: `stage`, `protocol`, `provider`, `event`, `data`, `raw`.
- Return values:
  - `nil`: Pass through (preserving in-place edits).
  - `{ action = "short_circuit", status?, headers?, body? }`: Respond locally and skip upstream invocation.
  - `{ action = "abort", message? }`: Reject the request with an error.
  - `{ action = "retry", delay_ms? }`: Re-send the upstream request after `delay_ms`. Consumed only on the error path of `on_upstream_response_raw` (`msg.error == true`), where it immediately starts a retry round and skips remaining plugins in the same stage. Capped by `pluginRetryMax` (default 8, 0 disables retries) and `pluginRetryDelayCapMs` (default 30000ms).
  - `{ action = "drop" }`: Chunk hooks only; drops the current SSE chunk.

Plugins that do not declare `raw_stream` incur zero Lua invocation overhead during streaming.

### Context and Host APIs (`mb`)

Each hook receives a read-only `ctx` table: `request_id`, `session_id`, `model_alias`, `client_protocol` (`openai-response` / `openai-chat` / `anthropic` / `google-genai`), `upstream_protocol`, `provider`, `stream`.

The global `mb` table provides host APIs (`http.request` and `provider.invoke` are async):

| API | Description |
|-----|------|
| `mb.log.debug / info / warn / error(msg)` | Structured logging tagged with the plugin name |
| `mb.config` | Plugin configuration table for the active binding |
| `mb.session.get(k)` / `mb.session.set(k, v)` | Session-scoped key-value store isolated by plugin name and session ID |
| `mb.http.request({ method, url, headers, body, timeout_ms })` | HTTP sub-request returning `{ status, headers, body }` (default 30s timeout, respects host egress proxy) |
| `mb.provider.invoke(provider, model, req)` | Invoke another provider using Core IR and return a `CoreResponse` |
| `mb.headers.get / set / remove(headers, name)` | Case-insensitive, order-preserving header helpers |
| `mb.crypto.sha256 / hmac_sha256 / base64_encode / base64_decode` | Hashing and Base64 helpers |

### Example Plugin

This plugin uses both `core` and `raw_request` capabilities to inject a system prompt, count requests per session, set an outbound header, and cap `max_tokens`:

```lua
MB = {
  version = "1.0.0",
  scopes = { "global", "provider" },
  capabilities = { "core", "raw_request" },
  config_schema = {
    type = "object",
    properties = {
      prefix     = { type = "string", title = "Hint injected into system" },
      max_tokens = { type = "number", title = "Cap for max_tokens" },
    },
  },
}

function MB.on_request(ctx, req)
  local n = (mb.session.get("count") or 0) + 1
  mb.session.set("count", n)
  mb.log.info(string.format("[%s] request #%d in session %s",
    ctx.model_alias, n, ctx.session_id or "anon"))
  table.insert(req.system, 1,
    { type = "text", text = mb.config.prefix or "Keep answers short." })
end

function MB.on_upstream_request_raw(ctx, msg)
  msg.headers = mb.headers.set(msg.headers, "x-moonbridge-session", ctx.session_id or "anon")
  if type(msg.body) == "table" then
    local cap = mb.config.max_tokens or 8192
    if msg.body.max_tokens and msg.body.max_tokens > cap then
      msg.body.max_tokens = cap
    end
  end
end
```

### Sandbox Limits and Reference Plugins

Each plugin runs in a dedicated, sandboxed Lua VM:
- Dangerous globals (`os`, `io`, `loadfile`, `dofile`, `require`, `package`) are removed.
- An instruction-count budget (200M instructions by default, enforced across plugin-created coroutines) and a wall-clock timeout terminate infinite loops and runaway scripts.
- Memory is capped at 1024 MB by default; payloads exceeding `max_body_bytes` (100 MB by default) are not decoded into Lua tables and are forwarded untouched with `body_truncated = true`.
- Uncaught plugin errors are logged as warnings and skipped without failing the request.

Bundled reference files:
- `plugins/examples/log_request.lua`: Semantic-layer example (logging, session counter, system prompt injection, error rewriting).
- `plugins/examples/raw_rewrite.lua`: Wire-layer example (auth check, header/body rewrites, heartbeat chunk dropping).
- `plugins/harness/*.lua`: Session identifier extraction plugins for popular coding agents.
- `plugins/utils/FxxkDax.lua`: Outbound session header injector.
- `plugins/utils/rescue_5xx.lua`, `plugins/utils/rescue_fake_200.lua`: Exponential-backoff retry plugins for upstream 5xx/429 and empty streaming responses.
- `plugins/moonbridge.lua`: LuaLS type stub. Add the `plugins` directory to `Lua.workspace.library` for autocompletion and type checking on `MB` and `mb`.

## Deployment & Security

### Desktop App

The desktop app provides full UI management (Dashboard, Providers, Models, Routes, Plugins, Usage, Quota, Traces, Settings) and system tray controls for showing the window, starting/stopping the gateway, and quitting.

### Headless Server (`moonbridge-server`)

For server and container environments, `moonbridge-server` serves the LLM gateway, the `/api/*` management REST API, and the frontend SPA on a single port:

- **Separate admin and gateway tokens**: Supply the admin token via `--admin-token` or `MOONBRIDGE_ADMIN_TOKEN` (used only for `/api/*`, held in memory, never persisted or echoed). Supply the gateway token via `--gateway-token`, `MOONBRIDGE_GATEWAY_TOKEN`, or the existing config file. The two tokens must be non-empty and distinct. The web UI keeps the admin token in page memory only, requiring re-authentication on page reload.
- **In-process restart**: Clicking "Restart gateway" drains in-flight requests, runs `shutdown`/`init` plugin hooks, and rebinds the listener in-process without relying on an external supervisor.
- **Container image**: The multi-stage `Dockerfile` builds the UI and server binary into a slim runtime image running as `uid 10001` (see `deploy/soul/` for a Docker Compose example). Place a TLS-terminating reverse proxy in front when exposing to untrusted networks.

### Credential Encryption and Backups

Provider API keys and quota configurations in SQLite are encrypted at rest with AES-GCM. The key file defaults to `<db>.key` (configurable via `--key-file` or `MOONBRIDGE_KEY_FILE`):
- On Unix, the key file is created with mode `0600`; on Windows, the key is wrapped with current-user DPAPI.
- Missing or mismatched keys fail closed on startup rather than falling back to plaintext.
- **Backups and rollback**: Always back up the `.db` database and `.key` file together. Keep a pre-upgrade backup before upgrading, as older releases cannot open databases migrated to newer encryption schemas.

### Quota Query Network Boundaries

HTTP requests made by quota plugins are restricted to prevent SSRF:
- Only same-origin or explicitly authorized public origins are allowed by default; private IP ranges and cloud metadata addresses are blocked.
- To allow internal quota endpoints, set `MOONBRIDGE_QUOTA_PRIVATE_ORIGINS` at startup to a JSON array of exact origins (e.g., `["https://quota.internal.example:8443"]`). Cloud metadata addresses remain blocked.
- Quota HTTP requests ignore system proxies, do not follow redirects, pin validated DNS addresses, cap decompressed responses at 1 MiB, and enforce a 30s per-request timeout (45s per provider). If `egressProxy` is configured on the gateway, quota HTTP requests return an explicit error rather than bypassing the proxy (inference requests continue to use the proxy).

## FAQ

<details>
<summary><strong>When do plugin changes take effect?</strong></summary>

Plugins are loaded when the gateway starts. After creating, editing, enabling, disabling, or deleting a plugin on the Plugins page, click "Restart gateway" to drain requests and reload plugins in-process.

</details>

<details>
<summary><strong>Why are no traces recorded?</strong></summary>

Traces are written to the `traces/` subdirectory under the app data directory by default. To record metadata without request/response bodies, disable "Record request/response bodies" in Settings. Only the most recent N trace files (500 by default) are retained by modification time.

</details>

<details>
<summary><strong>Where is application data stored?</strong></summary>

Desktop and server modes share the platform app data directory (`com.moonbridge.next`) by default, containing `moonbridge.db`, `moonbridge.db.key`, `plugins/`, and `traces/`. The headless server can override these paths via `--config-dir`, `--data-dir`, and `--key-file`.

</details>

## Documentation

- [docs/architecture.md](docs/architecture.md): Layered architecture, Core IR contract, hook semantics, database schema, and lifecycle details.
- [DEVELOPMENT.md](DEVELOPMENT.md): Prerequisites, build/test commands, and container deployment guide.

## License

Released under the [GPL-3.0-or-later](LICENSE) license.
