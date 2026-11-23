//! 定制化 tool 候选加权评分：三条规则的端到端契约（审计事实 → `/admin/tools/candidates`）。
//!
//! 断言直接落在真实实现上：事件按上游工具调用的真实载荷形状写进审计库，再走 HTTP 管理面，
//! 因此"事实抽取 → 纯函数打分 → 响应投影"整条链一起被锁住。

use crate::fixtures::*;
use crate::support::*;
use axum::http::StatusCode;
use serde_json::{Value, json};

/// 上游工具归属：SQL 族的候选都挂在这个 `(upstream_id, upstream_tool)` 下。
const SQL_UPSTREAM: (&str, &str) = ("ch", "query");

fn sql(table: &str) -> Value {
    json!({"sql": format!("SELECT id FROM {table} LIMIT 10")})
}

fn candidates_of(body: &Value) -> Vec<Value> {
    body["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .map(|c| c["subject"].clone())
        .collect()
}

fn find_candidate<'a>(body: &'a Value, subject: &str) -> &'a Value {
    body["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .find(|c| c["subject"] == json!(subject))
        .unwrap_or_else(|| panic!("候选里缺少 {subject}: {body}"))
}

/// 规则一：同一令牌在短窗口内连续查同一张表，后半段逐次加分（递增权重，非常数）。
///
/// 6 次 → ⌈6/2⌉=3，第 4/5/6 次分别 +1.5/+3.0/+4.5，合计 9.0；同时验证单次调用不成候选
/// （分数 0 的对象不占 `limit`），以及没有上游归属的内置工具事件不入选。
#[tokio::test]
async fn consecutive_refinement_weights_the_tail_of_a_segment() {
    let h = harness();
    for minutes in [60, 59, 58, 57, 56, 55] {
        upstream_call(
            &h.audit,
            minutes,
            "tok-a",
            "ok",
            sql("env_air.orders"),
            SQL_UPSTREAM.0,
            SQL_UPSTREAM.1,
        );
    }
    // 单次成功调用：没有任何收紧证据，不该出现在候选里。
    upstream_call(
        &h.audit,
        40,
        "tok-a",
        "ok",
        sql("env_air.customers"),
        SQL_UPSTREAM.0,
        SQL_UPSTREAM.1,
    );
    // 内置工具调用（无 upstream 归属）：即使连续查同一张表也不参与候选。
    for minutes in [30, 29, 28] {
        tool_call(&h.audit, minutes, "s-builtin", "ok", Some(10));
    }

    let (status, body) = call(&h.app, Req::get("/admin/tools/candidates")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["window"]["days"], json!(7), "默认窗口 7 天");
    assert_eq!(body["truncated"], json!(false));
    assert_eq!(
        body["weights"]["refine_step"],
        json!(1.5),
        "权重随结果回显，分数才可解释"
    );
    assert_eq!(body["weights"]["segment_gap_seconds"], json!(600));

    let orders = find_candidate(&body, "env_air.orders");
    assert_eq!(orders["family"], json!("sql"));
    assert_eq!(orders["upstream_id"], json!("ch"));
    assert_eq!(orders["upstream_tool"], json!("query"));
    assert_eq!(orders["calls"], json!(6));
    assert_eq!(orders["longest_segment"], json!(6));
    assert_eq!(orders["refined_calls"], json!(3), "只有后半段 3 次被加权");
    assert_eq!(orders["failures"], json!(0));
    assert_eq!(orders["distinct_tokens"], json!(1));
    assert_eq!(orders["score"], json!(9.0), "1.5+3.0+4.5");
    let reasons = orders["reasons"].as_array().expect("reasons");
    assert!(
        reasons
            .iter()
            .any(|r| r.as_str().unwrap_or_default().contains("后半段 3 次")),
        "{reasons:?}"
    );

    assert_eq!(
        candidates_of(&body),
        vec![json!("env_air.orders")],
        "单次调用与内置调用都不入候选"
    );
}

/// 规则二：失败的调用扣分，能压过短段收索引来的加分；扣成非正分的对象直接出局。
#[tokio::test]
async fn failed_calls_subtract_points() {
    let h = harness();
    // 干净对象：4 次连续成功 → 1.5+3.0 = 4.5。
    for minutes in [40, 39, 38, 37] {
        upstream_call(
            &h.audit,
            minutes,
            "tok-a",
            "ok",
            sql("env.clean"),
            SQL_UPSTREAM.0,
            SQL_UPSTREAM.1,
        );
    }
    // 抖动对象：4 次成功 + 1 次失败 → 段长 5，后半段 2 次 (1.5+3.0) 再扣 2.0 = 2.5。
    for minutes in [30, 29, 28, 27] {
        upstream_call(
            &h.audit,
            minutes,
            "tok-a",
            "ok",
            sql("env.flaky"),
            SQL_UPSTREAM.0,
            SQL_UPSTREAM.1,
        );
    }
    upstream_call(
        &h.audit,
        26,
        "tok-a",
        "error",
        sql("env.flaky"),
        SQL_UPSTREAM.0,
        SQL_UPSTREAM.1,
    );
    // 失败对象：一次成功 + 一次失败 → 1.5-2.0 < 0，整条出局。
    for (minutes, outcome) in [(20, "ok"), (19, "error")] {
        upstream_call(
            &h.audit,
            minutes,
            "tok-a",
            outcome,
            sql("env.broken"),
            SQL_UPSTREAM.0,
            SQL_UPSTREAM.1,
        );
    }

    let (status, body) = call(&h.app, Req::get("/admin/tools/candidates")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        candidates_of(&body),
        vec![json!("env.clean"), json!("env.flaky")],
        "干净对象排在抖动对象之前，净分为负的对象出局"
    );

    let clean = find_candidate(&body, "env.clean");
    assert_eq!(clean["score"], json!(4.5));
    assert_eq!(clean["failures"], json!(0));
    let flaky = find_candidate(&body, "env.flaky");
    assert_eq!(flaky["score"], json!(2.5), "4.5 - 2.0");
    assert_eq!(flaky["failures"], json!(1));
    assert_eq!(flaky["refined_calls"], json!(2));
    let reasons = flaky["reasons"].as_array().expect("reasons");
    assert!(
        reasons
            .iter()
            .any(|r| r.as_str().unwrap_or_default().contains("1 次调用失败")),
        "{reasons:?}"
    );
}

/// 规则三：不同令牌访问同一对象获得小幅加分（广度证据），但权重小于深度证据；
/// 加分封顶 6.0，避免"十个令牌各碰一次"盖过真正被反复收紧的对象。
#[tokio::test]
async fn distinct_tokens_add_a_small_capped_bonus() {
    let h = harness();
    // 两个令牌各 2 次连续调用：1.5+1.5 基础分 + 2.0 广度加分 = 5.0。
    for (token, minutes) in [("tok-a", 50), ("tok-a", 49), ("tok-b", 40), ("tok-b", 39)] {
        upstream_call(
            &h.audit,
            minutes,
            token,
            "ok",
            sql("env.shared"),
            SQL_UPSTREAM.0,
            SQL_UPSTREAM.1,
        );
    }
    // 单令牌 4 次连续收紧：1.5+3.0 = 4.5，没有广度加分。
    for minutes in [20, 19, 18, 17] {
        upstream_call(
            &h.audit,
            minutes,
            "tok-a",
            "ok",
            sql("env.single"),
            SQL_UPSTREAM.0,
            SQL_UPSTREAM.1,
        );
    }
    // 五个令牌各 2 次：(5-1)*2.0 封顶 6.0，加上 5 段各 1.5 的基础分。
    for (index, token) in ["tok-1", "tok-2", "tok-3", "tok-4", "tok-5"]
        .into_iter()
        .enumerate()
    {
        let base = 80 - index as i64 * 5;
        for minutes in [base, base - 1] {
            upstream_call(
                &h.audit,
                minutes,
                token,
                "ok",
                sql("env.crowded"),
                SQL_UPSTREAM.0,
                SQL_UPSTREAM.1,
            );
        }
    }

    let (status, body) = call(&h.app, Req::get("/admin/tools/candidates")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        candidates_of(&body),
        vec![
            json!("env.crowded"),
            json!("env.shared"),
            json!("env.single")
        ],
        "多令牌对象越过单令牌对象；广度加分封顶后不会无限膨胀"
    );

    let shared = find_candidate(&body, "env.shared");
    assert_eq!(shared["distinct_tokens"], json!(2));
    assert_eq!(shared["score"], json!(5.0), "1.5+1.5+2.0");
    assert_eq!(find_candidate(&body, "env.single")["score"], json!(4.5));

    let crowded = find_candidate(&body, "env.crowded");
    assert_eq!(crowded["distinct_tokens"], json!(5));
    assert_eq!(crowded["score"], json!(13.5), "5*1.5 + 封顶 6.0");
    let reasons = crowded["reasons"].as_array().expect("reasons");
    assert!(
        reasons
            .iter()
            .any(|r| r.as_str().unwrap_or_default().contains("加 6.0 分")),
        "{reasons:?}"
    );
}

/// 接口与文件对象按"对象"聚合：URL 的查询串是变化的条件，不是新的对象。
#[tokio::test]
async fn subjects_group_by_object_not_by_condition() {
    let h = harness();
    for (index, minutes) in [30, 29, 28].into_iter().enumerate() {
        upstream_call(
            &h.audit,
            minutes,
            "tok-a",
            "ok",
            json!({"url": format!("https://api.example.com/orders?page={}", index + 1)}),
            "crm",
            "http_get",
        );
    }
    for minutes in [20, 19] {
        upstream_call(
            &h.audit,
            minutes,
            "tok-a",
            "ok",
            json!({"path": "/data/exports/orders.csv"}),
            "fs",
            "read_file",
        );
    }
    // 无对象参数的上游工具：退回到上游工具全名，仍然是一条可固化的候选。
    for minutes in [10, 9] {
        upstream_call(
            &h.audit,
            minutes,
            "tok-a",
            "ok",
            json!({}),
            "crm",
            "list_database",
        );
    }

    let (status, body) = call(&h.app, Req::get("/admin/tools/candidates")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        candidates_of(&body),
        vec![
            json!("api.example.com/orders"),
            json!("/data/exports/orders.csv"),
            json!("crm/list_database")
        ],
        "查询串不得把同一接口拆成三个对象"
    );
    assert_eq!(
        find_candidate(&body, "api.example.com/orders")["family"],
        json!("http")
    );
    assert_eq!(
        find_candidate(&body, "/data/exports/orders.csv")["family"],
        json!("file")
    );
    assert_eq!(
        find_candidate(&body, "crm/list_database")["family"],
        json!("tool")
    );
}

/// 窗口/条数夹紧与未知参数拒绝：与统计台账同一口径，未支持的过滤键必须报错而不是静默忽略。
#[tokio::test]
async fn window_and_limit_are_clamped() {
    let h = harness();
    for minutes in [50, 49, 48] {
        upstream_call(
            &h.audit,
            minutes,
            "tok-a",
            "ok",
            sql("env.orders"),
            SQL_UPSTREAM.0,
            SQL_UPSTREAM.1,
        );
    }
    for minutes in [40, 39] {
        upstream_call(
            &h.audit,
            minutes,
            "tok-b",
            "ok",
            sql("env.payments"),
            SQL_UPSTREAM.0,
            SQL_UPSTREAM.1,
        );
    }

    let (status, body) = call(&h.app, Req::get("/admin/tools/candidates?window_days=0")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["window"]["days"], json!(1), "下界夹紧到 1");

    let (status, body) = call(&h.app, Req::get("/admin/tools/candidates?window_days=999")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["window"]["days"], json!(30), "上界夹紧到 30");

    let (status, body) = call(&h.app, Req::get("/admin/tools/candidates?limit=1")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["candidates"].as_array().expect("candidates").len(), 1);
    assert_eq!(body["truncated"], json!(true), "超过 limit 必须标记截断");
    assert_eq!(body["candidates"][0]["subject"], json!("env.orders"));

    let (status, body) = call(&h.app, Req::get("/admin/tools/candidates?bogus=1")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("MCP_FILTER_UNSUPPORTED"));
}

/// 管理台自己的调用（`surface="admin"`）不得进入候选：否则看板刷新两次就把自己刷成热点。
#[tokio::test]
async fn platform_surface_events_are_excluded() {
    let h = harness();
    for minutes in [10, 9, 8] {
        append_admin_event(
            &h.audit,
            "tool_call",
            minutes,
            "s-admin",
            json!({
                "object_type": "tool",
                "object_id": "ch__query",
                "action": "call",
                "outcome": "ok",
                "data": {
                    "arguments": {"sql": "SELECT id FROM env.admin_echo LIMIT 10"},
                    "upstream_id": "ch",
                    "upstream_tool": "query",
                    "elapsed_ms": 1
                }
            }),
        );
    }

    let (status, body) = call(&h.app, Req::get("/admin/tools/candidates")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["candidates"], json!([]));
}
