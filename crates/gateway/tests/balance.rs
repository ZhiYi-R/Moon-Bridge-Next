//! 余额&健康看板引擎集成测试：真实 Lua 脚本（含 `mb.http`）+ 内存库 + mock 上游。
//!
//! 覆盖写入 `crates/gateway/src/balance.rs` 的四层契约：脚本返回值的归一（quotas
//! camelCase + used/left 互补）、多 key 拆卡（逐 key 执行、按 key 拆行落库、掩码
//! label、失败按 key 独立分层、prune 残留行），以及定时调度「从未查询即到期」的
//! 选卡路径。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::routing::post;
use axum::{Json, Router};
use moonbridge_gateway::{run_balance_loop, BalanceEngine};
use moonbridge_store::{BalanceCard, BalanceKeyResult, Database, Endpoint, Provider};
use serde_json::{json, Value};

/// 成功脚本：给出 used_percent，验证引擎补齐 left_percent。
const SCRIPT_OK: &str = r#"
MB = {}
function MB.query(ctx)
  return {
    quotas = { { label = "Weekly", used_percent = 43, reset_at = "t" } },
    summary = "s",
  }
end
"#;

/// 起一个 mock 配额上游（`POST /quota`），返回 `{"left": 57}`；返回其 base_url。
async fn spawn_mock_quota() -> String {
    async fn quota() -> Json<Value> {
        Json(json!({ "left": 57 }))
    }

    let app = Router::new().route("/quota", post(quota));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn card(key: &str, script_ref: &str) -> BalanceCard {
    BalanceCard {
        key: key.to_string(),
        provider_key: None,
        display_mode: "auto".to_string(),
        api_key: "sk-test".to_string(),
        base_url: "https://api.example.test".to_string(),
        provider_label: "示例服务商".to_string(),
        script_ref: script_ref.to_string(),
        interval_secs: 60,
        enabled: true,
        extra: json!({"widget": "quota"}),
        position: 0,
        created_at: 0,
        updated_at: 0,
    }
}

/// 构造一个上游 Provider：`endpoints` 为 (base_url, api_key) 列表（协议随意，
/// 余额解析不读协议）。
fn provider(key: &str, endpoints: &[(&str, &str)]) -> Provider {
    Provider {
        key: key.to_string(),
        endpoints: endpoints
            .iter()
            .map(|(url, k)| Endpoint {
                protocol: "openai".to_string(),
                base_url: url.to_string(),
                api_key: k.to_string(),
            })
            .collect(),
        version: None,
        user_agent: None,
        web_search: None,
        extra: json!({}),
        enabled: true,
        created_at: 0,
        updated_at: 0,
    }
}

fn engine(db: &Arc<Database>, plugins_dir: Option<PathBuf>) -> BalanceEngine {
    BalanceEngine::new(db.clone(), plugins_dir)
}

/// 单 key 卡片（或取首行）的运行结果：引擎现在返回逐 key 数组，单 key 场景取 [0]。
fn only(results: &[BalanceKeyResult]) -> &BalanceKeyResult {
    assert_eq!(results.len(), 1, "单 key 卡片应只有一行结果: {results:?}");
    &results[0]
}

fn stored(db: &Database, key: &str, idx: i64) -> BalanceKeyResult {
    db.get_balance_key_result(key, idx)
        .unwrap()
        .expect("结果应已落库")
}

fn assert_close(actual: Option<f64>, expected: f64) {
    let v = actual.unwrap_or_else(|| panic!("缺少百分比字段，期望 {expected}"));
    assert!(
        (v - expected).abs() < 1e-9,
        "期望 {expected}，实际 {v}（互补值必须精确补齐）"
    );
}

#[tokio::test]
async fn run_card_success_normalizes_quotas() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let c = card("ok", SCRIPT_OK);
    let results = engine(&db, None).run_card(&c).await;
    let res = &only(&results).result;

    assert_eq!(res.status, "ok");
    assert_eq!(res.error, None);
    assert!(res.queried_at > 0);

    let payload = res.payload.clone().expect("成功结果应带 payload");
    assert_eq!(payload["summary"], json!("s"), "自定义字段原样保留");
    let quota = &payload["quotas"][0];
    assert_eq!(quota["label"], json!("Weekly"));
    assert_close(quota["usedPercent"].as_f64(), 43.0);
    assert_close(quota["leftPercent"].as_f64(), 57.0);
    assert_eq!(quota["resetAt"], json!("t"));
    assert!(
        quota.get("used_percent").is_none(),
        "归一后不得留下 snake_case 键: {quota}"
    );
    assert_eq!(
        payload["quotas"].as_array().map(Vec::len),
        Some(1),
        "quotas 不应被改写形状: {payload}"
    );

    // 落库结果与返回值一致（JSON 往返不做数值重排，仅尾数 0 可能消失）
    let roundtrip: Value = serde_json::from_str(&payload.to_string()).unwrap();
    assert_eq!(stored(&db, "ok", 0).result.payload, Some(roundtrip));
}

#[tokio::test]
async fn engine_failure_keeps_previous_payload() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let eng = engine(&db, None);

    let results = eng.run_card(&card("keep", SCRIPT_OK)).await;
    let first = &only(&results).result;
    assert_eq!(first.status, "ok");
    let good = first.payload.clone().expect("首次成功应有 payload");

    // 换成抛错脚本重跑：脚本读到但立即失败 → 引擎级失败
    let broken = card("keep", "error(\"上游不可达\")");
    let results = eng.run_card(&broken).await;
    let res = &only(&results).result;
    assert_eq!(res.status, "error");
    assert!(res.error.as_deref().is_some_and(|e| !e.is_empty()));
    assert_eq!(
        res.payload,
        Some(good.clone()),
        "引擎级失败必须保留上一次的 payload"
    );
    assert_eq!(stored(&db, "keep", 0).result.payload, Some(good));
    assert_eq!(stored(&db, "keep", 0).result.status, "error");
}

#[tokio::test]
async fn business_error_updates_payload() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let c = card(
        "biz",
        r#"
        MB = {}
        function MB.query(ctx)
          return { status = "error", message = "key 失效", note = "diag" }
        end
        "#,
    );
    let results = engine(&db, None).run_card(&c).await;
    let res = &only(&results).result;
    assert_eq!(res.status, "error");
    assert_eq!(
        res.error.as_deref(),
        Some("key 失效"),
        "error 取脚本 message"
    );
    let payload = res
        .payload
        .clone()
        .expect("脚本返回了 table，payload 应更新");
    assert_eq!(payload["note"], json!("diag"), "诊断字段必须保留");
}

#[tokio::test]
async fn missing_query_entry_is_engine_error() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let results = engine(&db, None)
        .run_card(&card(
            "noquery",
            "MB = {}\nfunction MB.other(ctx) return {} end",
        ))
        .await;
    let res = &only(&results).result;
    assert_eq!(res.status, "error");
    assert!(
        res.error.as_deref().is_some_and(|e| e.contains("MB.query")),
        "缺失入口应报出 MB.query: {:?}",
        res.error
    );
    assert_eq!(res.payload, None, "首次失败无可保留的旧 payload");
}

#[tokio::test]
async fn mb_http_real_roundtrip_complements_percent() {
    let base_url = spawn_mock_quota().await;
    let db = Arc::new(Database::open_in_memory().unwrap());
    let script = r#"
    MB = {}
    function MB.query(ctx)
      local r = mb.http.request({
        method = "POST",
        url = ctx.base_url .. "/quota",
        headers = { { "Authorization", "Bearer " .. ctx.key } },
        timeout_ms = 5000,
      })
      if r.status ~= 200 then
        return { status = "error", message = "上游 " .. tostring(r.status) }
      end
      return { quotas = { { label = "5h", left_percent = r.body.left } } }
    end
    "#;
    let mut c = card("http", script);
    c.base_url = base_url.clone();

    let results = engine(&db, None).run_card(&c).await;
    let res = &only(&results).result;
    assert_eq!(res.status, "ok", "error: {:?}", res.error);
    let quota = &res.payload.clone().expect("应有 payload")["quotas"][0];
    assert_eq!(quota["label"], json!("5h"));
    assert_close(quota["leftPercent"].as_f64(), 57.0);
    assert_close(quota["usedPercent"].as_f64(), 43.0);
}

#[tokio::test]
async fn script_ref_outside_plugins_dir_is_rejected() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let dir =
        std::env::temp_dir().join(format!("moonbridge-balance-plugins-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // 越界文件真实存在：拒绝必须来自路径包含性校验，而不是「读不到文件」
    let outside = std::env::temp_dir().join(format!(
        "moonbridge-balance-outside-{}.lua",
        std::process::id()
    ));
    std::fs::write(&outside, SCRIPT_OK).unwrap();

    let results = engine(&db, Some(dir.clone()))
        .run_card(&card("escape", outside.to_str().unwrap()))
        .await;
    let res = &only(&results).result;
    assert_eq!(res.status, "error");
    assert!(
        res.error
            .as_deref()
            .is_some_and(|e| e.contains("越出插件目录")),
        "应报路径越界: {:?}",
        res.error
    );

    let _ = std::fs::remove_file(&outside);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn scheduler_picks_up_never_queried_card() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    db.upsert_balance_card(&card("sched", SCRIPT_OK)).unwrap();
    assert!(
        db.list_balance_results("sched").unwrap().is_empty(),
        "调度前不应有结果行"
    );

    let task = tokio::spawn(run_balance_loop(
        db.clone(),
        engine(&db, None),
        Duration::from_millis(50),
    ));

    let mut landed = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let rows = db.list_balance_results("sched").unwrap();
        if let Some(r) = rows.into_iter().find(|r| r.result.status == "ok") {
            landed = Some(r);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    task.abort();

    let r = landed.expect("从未查询的启用卡片应在超时前被调度并落库");
    assert_eq!(
        r.result.payload.expect("应有 payload")["summary"],
        json!("s")
    );
}

#[tokio::test]
async fn amount_mode_quotas_pass_through_without_percent() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let script = r#"
    MB = {}
    function MB.query(ctx)
      return {
        quotas = {
          { label = "余额", unit = "¥", used_amount = 12.5, left_amount = 37.5 },
          { label = "流量", unit = "GB", used_amount = 3 },
        },
      }
    end
    "#;
    let results = engine(&db, None).run_card(&card("amount", script)).await;
    let res = &only(&results).result;
    assert_eq!(res.status, "ok", "error: {:?}", res.error);

    let payload = res.payload.clone().expect("成功结果应带 payload");
    let q0 = &payload["quotas"][0];
    assert_eq!(q0["label"], json!("余额"));
    assert_eq!(q0["unit"], json!("¥"));
    assert_close(q0["usedAmount"].as_f64(), 12.5);
    assert_close(q0["leftAmount"].as_f64(), 37.5);
    assert!(
        q0.get("usedPercent").is_none() && q0.get("leftPercent").is_none(),
        "金额模式不得生成 percent 字段: {q0}"
    );
    assert!(
        q0.get("used_amount").is_none(),
        "归一后不得留下 snake_case 键: {q0}"
    );

    // 只有 used_amount 时不得反向推出 left_amount 或 percent
    let q1 = &payload["quotas"][1];
    assert_close(q1["usedAmount"].as_f64(), 3.0);
    assert!(
        q1.get("leftAmount").is_none()
            && q1.get("usedPercent").is_none()
            && q1.get("leftPercent").is_none(),
        "amount 之间及与 percent 之间均不做互补互推: {q1}"
    );

    let roundtrip: Value = serde_json::from_str(&payload.to_string()).unwrap();
    assert_eq!(stored(&db, "amount", 0).result.payload, Some(roundtrip));
}

#[tokio::test]
async fn provider_card_splits_into_per_key_results() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    // 端点：sk-alpha-0001 → 留空（回退前一个）→ sk-beta-00002 → sk-alpha-0001（重复）
    db.upsert_provider(&provider(
        "kimi",
        &[
            ("https://a", "sk-alpha-0001"),
            ("https://b", ""),
            ("https://a2", "sk-beta-00002"),
            ("https://c", "sk-alpha-0001"),
        ],
    ))
    .unwrap();

    // 脚本按单 key 编写：summary 回显 ctx.key 与 ctx 形状，便于逐 key 断言
    let script = r#"
    MB = {}
    function MB.query(ctx)
      return {
        quotas = {},
        summary = ctx.key .. "|" .. ctx.keys[1]
          .. "|" .. ctx.base_url
          .. "|" .. tostring(#ctx.keys) .. "|" .. ctx.provider
          .. "|" .. tostring(ctx.base_urls == nil and ctx.endpoints == nil),
      }
    end
    "#;
    let mut c = card("ref", script);
    c.api_key.clear();
    c.provider_key = Some("kimi".to_string());
    c.provider_label = String::new(); // 留空 → 取 Provider key
    c.base_url = "https://quota.example".to_string(); // URL 是卡片自己的可选输入

    let results = engine(&db, None).run_card(&c).await;
    assert_eq!(results.len(), 2, "去重后的 2 个 key 各拆一行: {results:?}");

    let first = &results[0];
    assert_eq!(first.key_index, 0);
    assert_eq!(first.key_label, "sk-alp…0001", "key_label 是掩码而非原文");
    assert_eq!(first.result.status, "ok", "error: {:?}", first.result.error);
    assert_eq!(
        first.result.payload.as_ref().unwrap()["summary"],
        json!("sk-alpha-0001|sk-alpha-0001|https://quota.example|1|kimi|true"),
        "ctx 为单 key 形状、URL 取卡片输入、provider 回落、无 base_urls/endpoints"
    );

    let second = &results[1];
    assert_eq!(second.key_index, 1);
    assert_eq!(second.key_label, "sk-bet…0002");
    assert_eq!(
        second.result.payload.as_ref().unwrap()["summary"],
        json!("sk-beta-00002|sk-beta-00002|https://quota.example|1|kimi|true")
    );

    // 落库行与返回值一致；结果表里没有 key 原文
    let rows = db.list_balance_results("ref").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].key_label, "sk-alp…0001");
    assert_eq!(rows[1].key_label, "sk-bet…0002");
}

#[tokio::test]
async fn per_key_failure_is_independent() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    db.upsert_provider(&provider(
        "kimi",
        &[
            ("https://a", "sk-alpha-0001"),
            ("https://b", "sk-beta-00002"),
        ],
    ))
    .unwrap();
    // 只对第二个 key 抛错：引擎级失败按 key 独立，不拖垮整卡
    let script = r#"
    MB = {}
    function MB.query(ctx)
      if ctx.key == "sk-beta-00002" then
        error("boom")
      end
      return { quotas = {}, summary = ctx.key }
    end
    "#;
    let mut c = card("ind", script);
    c.api_key.clear();
    c.provider_key = Some("kimi".to_string());

    let results = engine(&db, None).run_card(&c).await;
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].result.status, "ok");
    assert_eq!(
        results[0].result.payload.as_ref().unwrap()["summary"],
        json!("sk-alpha-0001")
    );
    assert_eq!(results[1].result.status, "error");
    assert!(
        results[1]
            .result
            .error
            .as_deref()
            .is_some_and(|e| e.contains("boom")),
        "第二个 key 的错误应带脚本消息: {:?}",
        results[1].result.error
    );
    assert_eq!(
        results[1].result.payload, None,
        "首次失败无可保留的旧 payload"
    );
}

#[tokio::test]
async fn engine_failure_keeps_payload_per_key() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    db.upsert_provider(&provider(
        "kimi",
        &[
            ("https://a", "sk-alpha-0001"),
            ("https://b", "sk-beta-00002"),
        ],
    ))
    .unwrap();
    let eng = engine(&db, None);
    let mut c = card("keep2", SCRIPT_OK);
    c.api_key.clear();
    c.provider_key = Some("kimi".to_string());

    let results = eng.run_card(&c).await;
    assert_eq!(results.len(), 2);
    let good0 = results[0].result.payload.clone().expect("key0 首次成功");
    let good1 = results[1].result.payload.clone().expect("key1 首次成功");

    // 换成「只对 key1 抛错」的脚本：key0 照常更新，key1 保留旧 payload
    let mut broken = card(
        "keep2",
        r#"
    MB = {}
    function MB.query(ctx)
      if ctx.key == "sk-beta-00002" then error("boom") end
      return { quotas = {}, summary = "new-" .. ctx.key }
    end
    "#,
    );
    broken.api_key.clear();
    broken.provider_key = Some("kimi".to_string());
    let results = eng.run_card(&broken).await;
    assert_eq!(results[0].result.status, "ok");
    assert_eq!(
        results[0].result.payload.as_ref().unwrap()["summary"],
        json!("new-sk-alpha-0001"),
        "成功的 key 正常更新 payload"
    );
    assert_eq!(results[1].result.status, "error");
    assert_eq!(
        results[1].result.payload,
        Some(good1.clone()),
        "失败的 key 保留自己上一次的 payload"
    );
    assert_eq!(stored(&db, "keep2", 0).result.status, "ok");
    assert_eq!(stored(&db, "keep2", 1).result.payload, Some(good1));
    let _ = good0;
}

#[tokio::test]
async fn shrinking_keys_prunes_stale_rows() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    db.upsert_provider(&provider(
        "kimi",
        &[
            ("https://a", "sk-alpha-0001"),
            ("https://b", "sk-beta-00002"),
        ],
    ))
    .unwrap();
    let eng = engine(&db, None);
    let mut c = card("prune", SCRIPT_OK);
    c.api_key.clear();
    c.provider_key = Some("kimi".to_string());
    assert_eq!(eng.run_card(&c).await.len(), 2);

    // Provider 删到只剩一个 key：重跑后 idx1 残留行被剪掉
    db.upsert_provider(&provider("kimi", &[("https://a", "sk-alpha-0001")]))
        .unwrap();
    let results = eng.run_card(&c).await;
    assert_eq!(results.len(), 1);
    let rows = db.list_balance_results("prune").unwrap();
    assert_eq!(rows.len(), 1, "残留行应被 prune: {rows:?}");
    assert_eq!(rows[0].key_index, 0);
}

#[tokio::test]
async fn refresh_card_with_key_index_reruns_only_that_key() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    db.upsert_provider(&provider(
        "kimi",
        &[
            ("https://a", "sk-alpha-0001"),
            ("https://b", "sk-beta-00002"),
        ],
    ))
    .unwrap();
    let eng = engine(&db, None);
    let mut c = card("one", SCRIPT_OK);
    c.api_key.clear();
    c.provider_key = Some("kimi".to_string());
    db.upsert_balance_card(&c).unwrap();
    eng.run_card(&c).await;

    // 换脚本：key0 报错、key1 返回新 summary。只刷新 idx0 → idx1 保持旧值
    let mut changed = card(
        "one",
        r#"
    MB = {}
    function MB.query(ctx)
      if ctx.key == "sk-alpha-0001" then error("down") end
      return { quotas = {}, summary = "changed" }
    end
    "#,
    );
    changed.api_key.clear();
    changed.provider_key = Some("kimi".to_string());
    let view = eng.refresh_card(&changed, Some(0)).await;
    assert_eq!(view.results.len(), 2);
    assert_eq!(view.results[0].result.status, "error");
    assert!(
        view.results[0]
            .result
            .error
            .as_deref()
            .is_some_and(|e| e.contains("down")),
        "idx0 应被重跑成新错误: {:?}",
        view.results[0].result.error
    );
    assert_eq!(
        view.results[1].result.status, "ok",
        "idx1 不在刷新范围内，保持旧结果"
    );
    assert_eq!(
        view.results[1].result.payload.as_ref().unwrap()["summary"],
        json!("s"),
        "idx1 的 payload 仍是旧脚本的值"
    );

    // 越界 key_index：不动作，视图原样
    let view = eng.refresh_card(&changed, Some(9)).await;
    assert_eq!(view.results.len(), 2);
    assert_eq!(view.results[0].result.status, "error");

    // None：整卡重跑，idx1 也更新为新脚本的值
    let view = eng.refresh_card(&changed, None).await;
    assert_eq!(view.results[0].result.status, "error");
    assert_eq!(
        view.results[1].result.payload.as_ref().unwrap()["summary"],
        json!("changed"),
        "整卡刷新后 idx1 用新脚本重跑"
    );
}

#[tokio::test]
async fn provider_key_missing_is_single_error_row() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let mut c = card("ghost", SCRIPT_OK);
    c.api_key.clear();
    c.provider_key = Some("ghost".to_string());

    let results = engine(&db, None).run_card(&c).await;
    assert_eq!(results.len(), 1, "解析失败落单行错误结果");
    let kr = &results[0];
    assert_eq!(kr.key_index, 0);
    assert_eq!(kr.key_label, "", "无 key 时 label 为空串");
    assert_eq!(kr.result.status, "error");
    assert!(
        kr.result
            .error
            .as_deref()
            .is_some_and(|e| e.contains("上游服务 ghost 不存在")),
        "缺失 provider 应报引擎级错误: {:?}",
        kr.result.error
    );
    assert_eq!(kr.result.payload, None, "首次失败无可保留的旧 payload");

    // 已落库单行；若此前有多 key 残留行也被剪掉
    db.upsert_balance_key_result(
        "ghost",
        &BalanceKeyResult {
            key_index: 1,
            key_label: "old".to_string(),
            result: moonbridge_store::BalanceResult {
                status: "ok".to_string(),
                payload: Some(json!({"summary": "stale"})),
                error: None,
                queried_at: 1,
            },
        },
    )
    .unwrap();
    let results = engine(&db, None).run_card(&c).await;
    assert_eq!(results.len(), 1);
    assert_eq!(
        db.list_balance_results("ghost").unwrap().len(),
        1,
        "解析失败路径同样 prune 残留行"
    );
}

#[tokio::test]
async fn manual_keys_override_provider_across_query_paths() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    db.upsert_provider(&provider(
        "upstream",
        &[("https://unused", "provider-secret")],
    ))
    .unwrap();
    let script = r#"
    MB = {}
    function MB.query(ctx)
      assert(ctx.key ~= "provider-secret")
      assert(#ctx.keys == 1 and ctx.keys[1] == ctx.key)
      return { summary = ctx.key .. "|" .. ctx.provider }
    end
    "#;
    let eng = engine(&db, None);
    let mut c = card("manual", script);
    c.provider_key = Some("upstream".into());
    c.provider_label.clear();
    c.api_key = "  sk-manual-alpha\r\n\n sk-manual-beta \n sk-manual-alpha\n".into();
    db.upsert_balance_card(&c).unwrap();

    let preview = eng.test_card(&c).await;
    assert_eq!(preview.len(), 2);
    assert!(db.list_balance_results("manual").unwrap().is_empty());
    let results = eng.run_card(&c).await;
    assert_eq!(results.len(), 2);
    for (idx, key) in ["sk-manual-alpha", "sk-manual-beta"].iter().enumerate() {
        assert_eq!(results[idx].result.status, "ok");
        assert_eq!(results[idx].result.payload, preview[idx].result.payload);
        assert_eq!(
            results[idx].result.payload.as_ref().unwrap()["summary"],
            json!(format!("{key}|upstream"))
        );
        assert_ne!(results[idx].key_label, *key);
    }

    c.provider_key = Some("missing".into());
    db.upsert_balance_card(&c).unwrap();
    let refreshed = eng.refresh_card(&c, Some(1)).await;
    assert_eq!(
        refreshed.results[0].result.payload,
        results[0].result.payload
    );
    assert_eq!(refreshed.results[1].result.status, "ok");
    assert_eq!(
        refreshed.results[1].result.payload.as_ref().unwrap()["summary"],
        json!("sk-manual-beta|missing")
    );
    assert!(eng
        .test_card(&c)
        .await
        .iter()
        .all(|r| r.result.status == "ok"));

    c.api_key = "sk-manual-alpha".into();
    c.provider_key = None;
    c.provider_label = "manual-label".into();
    db.upsert_balance_card(&c).unwrap();
    let all = eng.refresh_all().await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].results.len(), 1);
    assert_eq!(
        all[0].results[0].result.payload.as_ref().unwrap()["summary"],
        json!("sk-manual-alpha|manual-label")
    );
    assert_eq!(db.list_balance_results("manual").unwrap().len(), 1);
}

#[tokio::test]
async fn whitespace_manual_keys_fall_back_to_provider() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    db.upsert_provider(&provider(
        "upstream",
        &[("https://unused", "from-provider")],
    ))
    .unwrap();
    let mut c = card(
        "fallback",
        r#"
    MB = {}
    function MB.query(ctx)
      assert(ctx.key == "from-provider")
      return { summary = "provider" }
    end
    "#,
    );
    c.api_key = " \r\n\t\n ".into();
    c.provider_key = Some("upstream".into());
    let eng = engine(&db, None);
    for results in [eng.test_card(&c).await, eng.run_card(&c).await] {
        assert_eq!(only(&results).result.status, "ok");
        assert_eq!(
            only(&results).result.payload.as_ref().unwrap()["summary"],
            json!("provider")
        );
    }
}

#[tokio::test]
async fn legacy_manual_card_gets_single_element_keys() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let script = r#"
    MB = {}
    function MB.query(ctx)
      return {
        quotas = {},
        summary = tostring(#ctx.keys) .. "/" .. ctx.keys[1]
          .. "/" .. ctx.base_url .. "|" .. ctx.provider,
      }
    end
    "#;
    let results = engine(&db, None).run_card(&card("legacy", script)).await;
    let kr = only(&results);
    assert_eq!(kr.key_label, "sk…", "短 key 掩码为前 2 位 + …");
    assert_eq!(kr.result.status, "ok", "error: {:?}", kr.result.error);
    let payload = kr.result.payload.clone().expect("应有 payload");
    let summary = payload["summary"].as_str().unwrap();
    assert_eq!(
        summary, "1/sk-test/https://api.example.test|示例服务商",
        "手填模式退化为单元素 keys，base_url 仍是卡片自己的输入: {summary}"
    );
}

#[tokio::test]
async fn empty_manual_key_still_runs_once() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    // 手填模式允许只填 URL（查公开接口）：空 key 回落为 [""]，卡片照常执行一次
    let script = r#"
    MB = {}
    function MB.query(ctx)
      return { quotas = {}, summary = "key=" .. ctx.key .. "/n=" .. tostring(#ctx.keys) }
    end
    "#;
    let mut c = card("nokey", script);
    c.api_key = String::new();
    let results = engine(&db, None).run_card(&c).await;
    let kr = only(&results);
    assert_eq!(kr.key_label, "", "空 key 的 label 为空串");
    assert_eq!(kr.result.status, "ok", "error: {:?}", kr.result.error);
    assert_eq!(
        kr.result.payload.as_ref().unwrap()["summary"],
        json!("key=/n=1")
    );
}

#[tokio::test]
async fn reset_at_accepts_number_and_string() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let script = r#"
    MB = {}
    function MB.query(ctx)
      return {
        quotas = {
          { label = "周窗口", used_percent = 43, reset_at = 1789616942 },
          { label = "月额度", used_percent = 10, reset_at = "每月 1 日" },
          { label = "坏值", used_percent = 5, reset_at = true },
        },
      }
    end
    "#;
    let results = engine(&db, None).run_card(&card("reset", script)).await;
    let res = &only(&results).result;
    assert_eq!(res.status, "ok", "error: {:?}", res.error);
    let quotas = res.payload.as_ref().unwrap()["quotas"]
        .as_array()
        .unwrap()
        .clone();
    // 数字（unix 秒）归一为字符串，前端据此按时间戳格式化
    assert_eq!(quotas[0]["resetAt"], json!("1789616942"));
    // 字符串原样透传
    assert_eq!(quotas[1]["resetAt"], json!("每月 1 日"));
    // 非法类型（bool）按 None 处理，不拖垮整条配额
    assert!(quotas[2].get("resetAt").is_none(), "{:?}", quotas[2]);
    assert_close(quotas[2]["leftPercent"].as_f64(), 95.0);
}

#[tokio::test]
async fn test_card_runs_without_persisting() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let eng = engine(&db, None);

    // 成功 dry-run：返回本次结果但不写库
    let results = eng.test_card(&card("dry", SCRIPT_OK)).await;
    let res = &only(&results).result;
    assert_eq!(res.status, "ok", "error: {:?}", res.error);
    assert!(res.payload.is_some());
    assert!(res.queried_at > 0);
    assert!(
        db.list_balance_results("dry").unwrap().is_empty(),
        "dry-run 不得把结果写库"
    );

    // 引擎级失败的 dry-run：payload 为 None（不读历史、不保留旧值）
    let results = eng.test_card(&card("dry", "error(\"上游不可达\")")).await;
    let res = &only(&results).result;
    assert_eq!(res.status, "error");
    assert!(res.error.as_deref().is_some_and(|e| !e.is_empty()));
    assert_eq!(res.payload, None, "dry-run 引擎级失败 payload 必须为 None");
    assert!(db.list_balance_results("dry").unwrap().is_empty());

    // 多 key 卡片的 dry-run：逐 key 返回，同样不落库
    db.upsert_provider(&provider(
        "kimi",
        &[
            ("https://a", "sk-alpha-0001"),
            ("https://b", "sk-beta-00002"),
        ],
    ))
    .unwrap();
    let mut c = card("drymulti", SCRIPT_OK);
    c.api_key.clear();
    c.provider_key = Some("kimi".to_string());
    let results = eng.test_card(&c).await;
    assert_eq!(results.len(), 2, "dry-run 也按 key 拆结果");
    assert_eq!(results[0].key_label, "sk-alp…0001");
    assert_eq!(results[1].key_label, "sk-bet…0002");
    assert!(db.list_balance_results("drymulti").unwrap().is_empty());
}

async fn run_builtin_template(
    name: &str,
    status: u16,
    body: Value,
    extra: Value,
) -> BalanceKeyResult {
    let source = include_str!("../../../ui/src/lib/balanceTemplates.ts");
    let marker = format!("const {name} = `");
    let script = source
        .split_once(&marker)
        .unwrap()
        .1
        .split_once("`;")
        .unwrap()
        .0;
    let path = match name {
        "NEW_API" => "/api/user/self",
        "KIMI_CODING" => "/coding/v1/usages",
        "COMMANDCODE" => "/alpha/billing/credits",
        "SUB2API" => "/v1/usage",
        _ => panic!("unknown template"),
    };
    let require_cli = matches!(name, "KIMI_CODING" | "COMMANDCODE");
    let require_json = name == "COMMANDCODE";
    let app = Router::new().route(
        path,
        axum::routing::get(move |headers: axum::http::HeaderMap| {
            let body = body.clone();
            async move {
                assert_eq!(headers.get("authorization").unwrap(), "Bearer sk-test");
                if require_cli {
                    assert_eq!(headers.get("user-agent").unwrap(), "cli");
                }
                if require_json {
                    assert_eq!(headers.get("content-type").unwrap(), "application/json");
                }
                (
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    Json(body),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let db = Arc::new(Database::open_in_memory().unwrap());
    let mut c = card("template", script);
    c.base_url = if name == "COMMANDCODE" {
        format!("http://{addr}{path}")
    } else if name == "SUB2API" {
        format!("  http://{addr}/v1/  ")
    } else {
        format!("http://{addr}/")
    };
    c.extra = extra;
    let results = engine(&db, None).test_card(&c).await;
    server.abort();
    only(&results).clone()
}

#[tokio::test]
async fn new_api_template_uses_account_quota_including_zero() {
    for (quota, expected) in [(12500000, 1.25), (0, 0.0)] {
        let result = run_builtin_template(
            "NEW_API",
            200,
            json!({"data": {"used_quota": quota}}),
            json!({}),
        )
        .await;
        assert_eq!(result.result.status, "ok");
        let payload = result.result.payload.unwrap();
        assert_eq!(payload["quotas"][0]["unit"], json!("¥"));
        assert_close(payload["quotas"][0]["leftAmount"].as_f64(), expected);
    }
    let result = run_builtin_template(
        "NEW_API",
        200,
        json!({"data": {"used_quota": "12500000"}}),
        json!({"quota_per_unit": 500000, "unit": "$"}),
    )
    .await;
    let payload = result.result.payload.unwrap();
    assert_close(payload["quotas"][0]["leftAmount"].as_f64(), 25.0);
    assert_eq!(payload["quotas"][0]["unit"], json!("$"));
}

#[tokio::test]
async fn kimi_template_uses_ratios_and_reset_times() {
    let result = run_builtin_template(
        "KIMI_CODING",
        200,
        json!({"usages": {
            "limit_5h": {"used_ratio": 0, "reset_time": "2026-09-17T12:00:00Z"},
            "limit_7d": {"used_ratio": 0.375, "reset_time": "2026-09-21T12:00:00Z"}
        }}),
        json!({}),
    )
    .await;
    assert_eq!(result.result.status, "ok");
    let payload = result.result.payload.unwrap();
    let quotas = payload["quotas"].as_array().unwrap();
    assert_eq!(quotas.len(), 2);
    assert_close(quotas[0]["usedPercent"].as_f64(), 0.0);
    assert_close(quotas[0]["leftPercent"].as_f64(), 100.0);
    assert_close(quotas[1]["usedPercent"].as_f64(), 37.5);
    assert_eq!(quotas[0]["resetAt"], json!("2026-09-17T12:00:00Z"));
    assert_eq!(quotas[1]["resetAt"], json!("2026-09-21T12:00:00Z"));
}

#[tokio::test]
async fn commandcode_template_uses_monthly_and_window_amounts() {
    let result = run_builtin_template(
        "COMMANDCODE",
        200,
        json!({
            "credits": {"monthlyCredits": 52.345},
            "windowLimits": {
                "fiveHour": {"used": 0, "cap": 14, "resetAt": 1800000000123_i64},
                "weekly": {"used": 36, "cap": 35}
            }
        }),
        json!({}),
    )
    .await;
    assert_eq!(result.result.status, "ok");
    let payload = result.result.payload.unwrap();
    let quotas = payload["quotas"].as_array().unwrap();
    assert_eq!(quotas.len(), 3);
    assert_close(quotas[0]["leftAmount"].as_f64(), 52.35);
    assert_close(quotas[0]["usedAmount"].as_f64(), 17.66);
    assert_eq!(quotas[0]["unit"], json!(""));
    assert_close(quotas[1]["leftAmount"].as_f64(), 14.0);
    assert_eq!(quotas[1]["resetAt"], json!("1800000000"));
    assert_close(quotas[2]["leftAmount"].as_f64(), 0.0);
    let result = run_builtin_template(
        "COMMANDCODE",
        200,
        json!({"credits": {"monthlyCredits": 0}}),
        json!({"monthly_cap": 100, "unit": "$"}),
    )
    .await;
    let payload = result.result.payload.unwrap();
    assert_close(payload["quotas"][0]["usedAmount"].as_f64(), 100.0);
    assert_close(payload["quotas"][0]["leftAmount"].as_f64(), 0.0);
    assert_eq!(payload["quotas"][0]["unit"], json!("$"));
    let result = run_builtin_template(
        "COMMANDCODE",
        200,
        json!({"windowLimits": {"weekly": {"used": 1, "cap": 2}}}),
        json!({}),
    )
    .await;
    assert_eq!(result.result.status, "ok");
    assert!(result.result.payload.unwrap()["summary"].is_null());
}

#[tokio::test]
async fn builtin_templates_report_http_and_schema_errors() {
    for name in ["NEW_API", "KIMI_CODING", "COMMANDCODE", "SUB2API"] {
        for (status, body) in [
            (401, json!({})),
            (200, json!({})),
            (200, json!("not an object")),
        ] {
            let result = run_builtin_template(name, status, body, json!({})).await;
            assert_eq!(result.result.status, "error", "{name}");
            let payload = result
                .result
                .payload
                .expect("business error must retain its payload");
            let message = payload["message"].as_str().unwrap();
            assert!(!message.is_empty());
            assert_eq!(result.result.error.as_deref(), Some(message));
        }
    }
    for (name, body, extra) in [
        (
            "NEW_API",
            json!({"success": false, "message": "access denied"}),
            json!({}),
        ),
        ("NEW_API", json!({}), json!({"quota_per_unit": 0})),
        (
            "KIMI_CODING",
            json!({"usages": {"limit_5h": {"used_ratio": 0}}}),
            json!({}),
        ),
        (
            "COMMANDCODE",
            json!({"windowLimits": {"weekly": {"used": 1}}}),
            json!({}),
        ),
        ("COMMANDCODE", json!({}), json!({"monthly_cap": 0})),
    ] {
        let result = run_builtin_template(name, 200, body, extra).await;
        assert_eq!(result.result.status, "error", "{name}");
        let payload = result
            .result
            .payload
            .expect("business error must retain its payload");
        assert_eq!(result.result.error.as_deref(), payload["message"].as_str());
        assert!(!payload["message"].as_str().unwrap().is_empty());
    }
}

#[tokio::test]
async fn sub2api_template_preserves_balance_precedence_and_windows() {
    for (body, label, amount) in [
        (
            json!({"quota": {"remaining": "0"}, "balance": 99, "remaining": 100}),
            "Key 剩余额度",
            0.0,
        ),
        (
            json!({"quota": {}, "balance": "12.5", "remaining": 100}),
            "钱包余额",
            12.5,
        ),
        (
            json!({"mode": "quota_limited", "remaining": 3}),
            "Key 剩余额度",
            3.0,
        ),
        (
            json!({"planName": "钱包余额", "remaining": -1}),
            "钱包余额",
            -1.0,
        ),
        (
            json!({"planName": "Pro", "remaining": 7}),
            "Pro 剩余额度",
            7.0,
        ),
        (json!({"remaining": 8}), "剩余额度", 8.0),
    ] {
        let result = run_builtin_template("SUB2API", 200, body, json!({})).await;
        assert_eq!(result.result.status, "ok");
        let payload = result.result.payload.unwrap();
        let quotas = payload["quotas"].as_array().unwrap();
        assert_eq!(quotas.len(), 1);
        assert_eq!(quotas[0]["label"], json!(label));
        assert_eq!(quotas[0]["unit"], json!("$"));
        assert_close(quotas[0]["leftAmount"].as_f64(), amount);
    }
    let result = run_builtin_template(
        "SUB2API",
        200,
        json!({"rate_limits": [
            {"window": "5h", "remaining": 0},
            {"window": "1d", "remaining": "1.5"},
            {"window": "7d", "remaining": 2},
            {"window": "30d", "remaining": 3},
            {"window": "bad", "remaining": "invalid"},
            "invalid"
        ]}),
        json!({}),
    )
    .await;
    assert_eq!(result.result.status, "ok");
    let payload = result.result.payload.unwrap();
    let quotas = payload["quotas"].as_array().unwrap();
    assert_eq!(quotas.len(), 4);
    for (i, label) in ["5 小时", "1 天", "7 天", "30d"].iter().enumerate() {
        assert_eq!(quotas[i]["label"], json!(format!("{label}周期剩余额度")));
    }
    assert_close(quotas[0]["leftAmount"].as_f64(), 0.0);
}

#[tokio::test]
async fn sub2api_template_reports_invalid_key_and_access_errors() {
    for (status, body, expected) in [
        (200, json!({"isValid": false, "balance": 100}), "Key 不可用"),
        (401, json!({}), "API Key 无效"),
        (403, json!({}), "访问被拒绝"),
        (404, json!({}), "/v1/usage"),
    ] {
        let result = run_builtin_template("SUB2API", status, body, json!({})).await;
        assert_eq!(result.result.status, "error");
        assert!(result.result.payload.unwrap()["message"]
            .as_str()
            .unwrap()
            .contains(expected));
    }
}
