use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{ensure, Context, Result};
use axum::{extract::State, http::HeaderMap, routing::get, Json, Router};
use reqwest::{Client, Method, StatusCode};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const ADMIN: &str = "synthetic-lifecycle-admin-token";
const GATEWAY: &str = "synthetic-lifecycle-gateway-token";
const PROVIDER_KEY: &str = "synthetic-lifecycle-provider-key";
const BALANCE_KEY: &str = "synthetic-lifecycle-balance-key";
const DEADLINE: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(25);
const SCRIPT: &str = r#"
MB = {}
function MB.query(ctx)
  local r = mb.http.request({
    method = "GET", url = ctx.base_url .. "/quota",
    headers = {{"authorization", "Bearer " .. ctx.key}}, timeout_ms = 10000
  })
  if r.status ~= 200 then return {status = "error", message = "query failed"} end
  return {status = "ok", quotas = {{label = "balance", left_amount = r.body.left, unit = "$"}}}
end
"#;

struct Task<T>(JoinHandle<T>);

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Directory(PathBuf);

impl Directory {
    fn new() -> Result<Self> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = std::env::temp_dir().join(format!(
            "moonbridge-lifecycle-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Process {
    child: Child,
    output: PathBuf,
}

impl Process {
    fn spawn(dir: &Path, addr: SocketAddr, origin: &str, generation: u8) -> Result<Self> {
        let output = dir.join(format!("server-{generation}.log"));
        let log = std::fs::File::create(&output)?;
        let child = Command::new(env!("CARGO_BIN_EXE_moonbridge-server"))
            .env_clear()
            .env("HOME", dir)
            .env("USERPROFILE", dir)
            .env("MOONBRIDGE_ADMIN_TOKEN", ADMIN)
            .env("MOONBRIDGE_GATEWAY_TOKEN", GATEWAY)
            .env(
                "MOONBRIDGE_BALANCE_PRIVATE_ORIGINS",
                serde_json::to_string(&[origin]).unwrap(),
            )
            .env("RUST_LOG", "info")
            .arg("--addr")
            .arg(addr.to_string())
            .arg("--config-dir")
            .arg(dir.join("config"))
            .arg("--data-dir")
            .arg(dir.join("data"))
            .arg("--web-dir")
            .arg(dir.join("web"))
            .arg("--key-file")
            .arg(dir.join("master.key"))
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()
            .context("spawn real server binary")?;
        Ok(Self { child, output })
    }

    fn diagnostics(&self) -> String {
        std::fs::read_to_string(&self.output).unwrap_or_default()
    }

    fn alive(&mut self) -> Result<()> {
        ensure!(
            self.child.try_wait()?.is_none(),
            "server exited unexpectedly"
        );
        Ok(())
    }

    async fn wait_exit(&mut self) -> Result<ExitStatus> {
        timeout(DEADLINE, async {
            loop {
                if let Some(status) = self.child.try_wait()? {
                    return Ok(status);
                }
                tokio::time::sleep(POLL).await;
            }
        })
        .await
        .context("process exit deadline exceeded")?
    }

    async fn stop(&mut self) -> Result<()> {
        self.alive()?;
        #[cfg(unix)]
        {
            let mut signal = Process {
                child: Command::new("/bin/kill")
                    .args(["-TERM", &self.child.id().to_string()])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .context("send SIGTERM")?,
                output: self.output.clone(),
            };
            ensure!(signal.wait_exit().await?.success(), "kill -TERM failed");
            ensure!(
                self.wait_exit().await?.success(),
                "SIGTERM must exit gracefully"
            );
        }
        #[cfg(not(unix))]
        {
            self.child.kill()?;
            self.wait_exit().await?;
        }
        Ok(())
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let deadline = std::time::Instant::now() + DEADLINE;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => break,
            }
        }
    }
}

#[derive(Default)]
struct Mock {
    hold: AtomicBool,
    entered: Notify,
    release: Notify,
}

async fn quota(State(state): State<Arc<Mock>>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
    if headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some(format!("Bearer {BALANCE_KEY}").as_str())
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"message": "wrong synthetic key"})),
        );
    }
    if state.hold.load(Ordering::SeqCst) {
        state.entered.notify_one();
        if timeout(Duration::from_secs(8), state.release.notified())
            .await
            .is_err()
        {
            return (StatusCode::GATEWAY_TIMEOUT, Json(Value::Null));
        }
    }
    (StatusCode::OK, Json(json!({"left": 12.5})))
}

async fn request(
    client: &Client,
    base: &str,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> Result<(StatusCode, String)> {
    let mut request = client.request(method, format!("{base}{path}"));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .await
        .with_context(|| format!("HTTP {path}"))?;
    let status = response.status();
    Ok((status, response.text().await?))
}

async fn admin(
    client: &Client,
    base: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value> {
    let (status, text) = request(client, base, method, path, Some(ADMIN), body).await?;
    ensure!(status == StatusCode::OK, "{path}: {status}: {text}");
    serde_json::from_str(&text).with_context(|| format!("JSON {path}: {text}"))
}

async fn ready(process: &mut Process, client: &Client, addr: SocketAddr) -> Result<()> {
    let base = format!("http://{addr}");
    timeout(DEADLINE, async {
        loop {
            process.alive()?;
            if let Ok((StatusCode::OK, text)) = request(
                client,
                &base,
                Method::GET,
                "/api/gateway/status",
                Some(ADMIN),
                None,
            )
            .await
            {
                let state: Value = serde_json::from_str(&text)?;
                if state["phase"] == "running" && state["running"] == true {
                    ensure!(
                        state["addr"] == addr.to_string(),
                        "unexpected listener: {state}"
                    );
                    return Ok(());
                }
            }
            tokio::time::sleep(POLL).await;
        }
    })
    .await
    .context("listener readiness deadline exceeded")?
}

async fn listener_closed(addr: SocketAddr) -> Result<()> {
    timeout(Duration::from_secs(3), async {
        loop {
            if let Err(error) = tokio::net::TcpStream::connect(addr).await {
                if error.kind() == std::io::ErrorKind::ConnectionRefused {
                    return;
                }
            }
            tokio::time::sleep(POLL).await;
        }
    })
    .await
    .context("listener did not close while draining")?;
    Ok(())
}

fn encrypted_storage(dir: &Path) -> Result<()> {
    ensure!(
        dir.join("master.key").is_file(),
        "server did not create its encryption key"
    );
    ensure!(
        std::fs::metadata(dir.join("data/moonbridge.db"))?.len() > 0,
        "empty database"
    );
    for entry in std::fs::read_dir(dir.join("data"))? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("moonbridge.db")
            && entry.file_type()?.is_file()
        {
            let bytes = std::fs::read(entry.path())?;
            for secret in [PROVIDER_KEY, BALANCE_KEY] {
                ensure!(
                    !bytes
                        .windows(secret.len())
                        .any(|window| window == secret.as_bytes()),
                    "plaintext credential in {}",
                    entry.path().display()
                );
            }
        }
    }
    Ok(())
}

fn valid_quota(results: &Value) -> Result<()> {
    let results = results
        .as_array()
        .context("balance results must be an array")?;
    ensure!(
        results.len() == 1,
        "manual key must override provider key: {results:?}"
    );
    let result = &results[0];
    ensure!(
        result["status"] == "ok" && result["error"].is_null(),
        "balance failed: {result}"
    );
    let quota = &result["payload"]["quotas"][0];
    ensure!(
        quota["label"] == "balance" && quota["unit"] == "$",
        "invalid quota: {quota}"
    );
    ensure!(
        quota["leftAmount"].as_f64() == Some(12.5),
        "invalid amount: {quota}"
    );
    Ok(())
}

async fn read_back(client: &Client, base: &str, card: &Value) -> Result<()> {
    let provider = admin(client, base, Method::GET, "/api/providers/lifecycle", None).await?;
    ensure!(
        provider["endpoints"][0]["apiKey"] == PROVIDER_KEY,
        "provider key did not decrypt"
    );
    let model = admin(
        client,
        base,
        Method::GET,
        "/api/models/lifecycle-model",
        None,
    )
    .await?;
    ensure!(
        model["displayName"] == "Lifecycle Model",
        "model did not persist"
    );
    let cards = admin(client, base, Method::GET, "/api/balance/cards", None).await?;
    let cards = cards.as_array().context("cards must be an array")?;
    ensure!(cards.len() == 1, "unexpected cards: {cards:?}");
    ensure!(
        cards[0]["apiKey"] == BALANCE_KEY,
        "manual balance key did not decrypt"
    );
    ensure!(
        cards[0]["scriptRef"] == card["scriptRef"],
        "balance script did not persist"
    );
    let refreshed = admin(
        client,
        base,
        Method::POST,
        "/api/balance/cards/manual/refresh",
        Some(Value::Null),
    )
    .await?;
    valid_quota(&refreshed["results"])
}

async fn exercise(
    process: &mut Process,
    dir: &Path,
    addr: SocketAddr,
    origin: &str,
    mock: Arc<Mock>,
) -> Result<()> {
    let client = Client::builder()
        .no_proxy()
        .pool_max_idle_per_host(0)
        .timeout(DEADLINE)
        .build()?;
    let probe = Client::builder()
        .no_proxy()
        .pool_max_idle_per_host(0)
        .timeout(Duration::from_millis(300))
        .build()?;
    let base = format!("http://{addr}");
    ready(process, &probe, addr).await?;
    for path in ["/health", "/models", "/v1/models"] {
        for token in [None, Some("synthetic-wrong-token")] {
            let (status, text) = request(&client, &base, Method::GET, path, token, None).await?;
            ensure!(status == StatusCode::OK, "public {path}: {status}: {text}");
            if path != "/health" {
                ensure!(
                    serde_json::from_str::<Value>(&text)?["data"].is_array(),
                    "not a model list: {text}"
                );
            }
        }
    }
    for (method, path) in [
        (Method::GET, "/api/config"),
        (Method::POST, "/api/gateway/restart"),
    ] {
        let (status, text) = request(&client, &base, method, path, Some(GATEWAY), None).await?;
        ensure!(
            status == StatusCode::UNAUTHORIZED,
            "gateway accessed admin {path}: {status}: {text}"
        );
    }
    for path in [
        "/v1/chat/completions",
        "/v1/responses",
        "/responses",
        "/v1/messages",
    ] {
        let body = json!({"model": "missing-model", "messages": [{"role": "user", "content": "synthetic"}], "input": "synthetic", "max_tokens": 1});
        let (status, text) =
            request(&client, &base, Method::POST, path, Some(ADMIN), Some(body)).await?;
        ensure!(
            status == StatusCode::UNAUTHORIZED,
            "admin accessed inference {path}: {status}: {text}"
        );
    }
    let mut config = admin(&client, &base, Method::GET, "/api/config", None).await?;
    ensure!(
        config["gateway"]["authToken"].is_null(),
        "GET config exposed authToken"
    );
    for secret in [ADMIN, GATEWAY] {
        ensure!(
            !config.to_string().contains(secret),
            "GET config exposed a credential"
        );
    }
    config["logLevel"] = json!("warn");
    admin(&client, &base, Method::PUT, "/api/config", Some(config)).await?;
    let saved = std::fs::read_to_string(dir.join("config/config.toml"))?;
    for secret in [ADMIN, GATEWAY] {
        ensure!(
            !saved.contains(secret),
            "config save persisted an environment credential"
        );
    }
    admin(&client, &base, Method::PUT, "/api/providers", Some(json!({
        "key": "lifecycle", "endpoints": [{"protocol": "openai-chat", "baseUrl": origin, "apiKey": PROVIDER_KEY}]
    }))).await?;
    admin(
        &client,
        &base,
        Method::PUT,
        "/api/models",
        Some(json!({
            "slug": "lifecycle-model", "displayName": "Lifecycle Model"
        })),
    )
    .await?;
    let card = json!({
        "key": "manual", "providerKey": "lifecycle", "apiKey": BALANCE_KEY,
        "baseUrl": origin, "scriptRef": SCRIPT, "intervalSecs": 0, "enabled": true
    });
    admin(
        &client,
        &base,
        Method::PUT,
        "/api/balance/cards",
        Some(card.clone()),
    )
    .await?;
    read_back(&client, &base, &card).await?;
    encrypted_storage(dir)?;

    mock.hold.store(true, Ordering::SeqCst);
    let slow_client = client.clone();
    let slow_base = base.clone();
    let slow_card = card.clone();
    let mut slow = Task(tokio::spawn(async move {
        admin(
            &slow_client,
            &slow_base,
            Method::POST,
            "/api/balance/test",
            Some(slow_card),
        )
        .await
    }));
    timeout(Duration::from_secs(5), async {
        tokio::select! {
            _ = mock.entered.notified() => Ok(()),
            result = &mut slow.0 => anyhow::bail!("balance request ended before entering mock: {result:?}"),
        }
    }).await.context("slow request did not enter mock")??;
    let pid = process.child.id();
    let (status, text) = request(
        &client,
        &base,
        Method::POST,
        "/api/gateway/restart",
        Some(ADMIN),
        Some(Value::Null),
    )
    .await?;
    ensure!(
        status == StatusCode::ACCEPTED,
        "restart rejected: {status}: {text}"
    );
    listener_closed(addr).await?;
    process.alive()?;
    ensure!(
        !slow.0.is_finished(),
        "restart truncated the held balance request"
    );
    mock.hold.store(false, Ordering::SeqCst);
    mock.release.notify_one();
    let results = timeout(DEADLINE, &mut slow.0)
        .await
        .context("slow response deadline")?
        .context("slow task join failed")??;
    valid_quota(&results)?;
    ready(process, &probe, addr).await?;
    ensure!(
        process.child.id() == pid,
        "HTTP restart replaced the process"
    );
    process.alive()?;
    read_back(&client, &base, &card).await?;

    process.stop().await?;
    listener_closed(addr).await?;
    encrypted_storage(dir)?;
    *process = Process::spawn(dir, addr, origin, 2)?;
    ready(process, &probe, addr).await?;
    read_back(&client, &base, &card).await?;
    let config = admin(&client, &base, Method::GET, "/api/config", None).await?;
    ensure!(
        config["logLevel"] == "warn",
        "saved config did not survive process restart"
    );
    ensure!(
        config["gateway"]["authToken"].is_null(),
        "reopened config exposed a token"
    );
    process.stop().await?;
    listener_closed(addr).await?;
    encrypted_storage(dir)?;
    Ok(())
}

#[tokio::test]
async fn real_http_restart_drains_and_encrypted_state_survives_process_reopen() {
    let dir = Directory::new().expect("create isolated test directory");
    let mock = Arc::new(Mock::default());
    let listener = timeout(DEADLINE, tokio::net::TcpListener::bind("127.0.0.1:0"))
        .await
        .expect("mock bind deadline")
        .expect("bind mock");
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new()
        .route("/quota", get(quota))
        .with_state(mock.clone());
    let mut mock_task = Task(tokio::spawn(
        async move { axum::serve(listener, router).await },
    ));
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve server port");
    let addr = reservation.local_addr().unwrap();
    drop(reservation);
    let mut process = Process::spawn(&dir.0, addr, &origin, 1).expect("spawn test server");
    let result = timeout(
        Duration::from_secs(120),
        exercise(&mut process, &dir.0, addr, &origin, mock),
    )
    .await;
    mock_task.0.abort();
    let _ = timeout(DEADLINE, &mut mock_task.0).await;
    if !matches!(&result, Ok(Ok(()))) {
        panic!(
            "real HTTP lifecycle failed: {result:?}\n{}",
            process.diagnostics()
        );
    }
}
