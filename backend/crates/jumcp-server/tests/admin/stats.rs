use crate::fixtures::*;
use crate::support::*;
use axum::http::StatusCode;
use jumcp_core::registry::BUILTIN_CAPABILITY_IDS;
use serde_json::{Value, json};

#[tokio::test]
async fn stats_aggregates_calls_success_retries_and_token_samples() {
    let h = harness();
    seed_template_capability(&h.store, "order.daily_summary");
    // 同一 session：ok(miss) → ok(hit) → error → error(budget)。
    // error 之后同 session 还有一次调用，因此计 1 次重试（budget error 不是"重试源"）。
    template_run(
        &h.audit,
        "order.daily_summary",
        50,
        "s-1",
        "ok",
        "miss",
        Some(300),
    );
    template_run(
        &h.audit,
        "order.daily_summary",
        40,
        "s-1",
        "ok",
        "hit",
        Some(500),
    );
    template_run(
        &h.audit,
        "order.daily_summary",
        30,
        "s-1",
        "error",
        "miss",
        None,
    );
    template_run(
        &h.audit,
        "order.daily_summary",
        20,
        "s-1",
        "error",
        "miss",
        None,
    );
    tool_call(&h.audit, 10, "s-2", "ok", Some(100));

    let (status, body) = call(&h.app, Req::get("/admin/tools/stats")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["window"]["days"], json!(7), "window_days 默认 7");
    assert!(body["window"]["from"].as_str().is_some());
    assert!(body["window"]["to"].as_str().is_some());
    assert_eq!(body["truncated"], json!(false));

    let tpl = find_tool(&body, "order.daily_summary");
    assert_eq!(tpl["kind"], json!("template"));
    assert_eq!(tpl["group"], json!("db.read.core"));
    assert_eq!(tpl["lifecycle_status"], json!("draft"));
    assert_eq!(tpl["risk"], json!("read"));
    assert_eq!(tpl["discoverable"], json!(false));
    assert_eq!(tpl["callable"], json!(false));
    assert_eq!(tpl["version"], json!(1));
    assert!(tpl["last_changed_at"].as_str().is_some());
    assert_eq!(tpl["source"], json!("managed"));
    assert!(tpl.get("status").is_none(), "旧字段 status 必须删除");
    assert_eq!(tpl["calls"], json!(4));
    assert_eq!(tpl["success"], json!(2), "ok 与 summary 计入 success");
    assert_eq!(
        tpl["failure"],
        json!(2),
        "error 与 budget_rejected 计入 failure"
    );
    assert_eq!(tpl["success_rate"], json!(0.5));
    assert_eq!(tpl["cache_hits"], json!(1));
    assert_eq!(tpl["hit_rate"], json!(0.25));
    assert_eq!(
        tpl["retries"],
        json!(1),
        "error 后同 session 还有调用 → 1 次重试"
    );
    assert_eq!(tpl["avg_response_tokens"], json!(400.0), "(300+500)/2");
    assert_eq!(tpl["p95_response_tokens"], json!(500), "最近秩法 n=2");
    assert_eq!(tpl["max_response_tokens"], json!(500));
    assert_eq!(tpl["recent_tokens"], json!([300, 500]), "时间升序");
    assert_eq!(tpl["last_outcome"], json!("error"), "取时间最新一次");
    assert!(tpl["last_call_at"].as_str().is_some());
    assert_eq!(tpl["health"], json!("failing"), "success_rate 0.5 < 0.80");

    let builtin = find_tool(&body, "search_capabilities");
    assert_eq!(builtin["kind"], json!("tool"));
    assert_eq!(builtin["lifecycle_status"], json!("published"));
    assert_eq!(builtin["risk"], json!("read"));
    assert_eq!(builtin["discoverable"], json!(true));
    assert_eq!(builtin["callable"], json!(true));
    assert_eq!(builtin["version"], Value::Null);
    assert_eq!(builtin["last_changed_at"], Value::Null);
    assert_eq!(builtin["source"], json!("builtin"));
    assert_eq!(builtin["calls"], json!(1));
    assert_eq!(builtin["success_rate"], json!(1.0));
    assert_eq!(builtin["hit_rate"], Value::Null, "非 template 不给命中率");
    assert_eq!(builtin["health"], json!("ok"));
}

#[tokio::test]
async fn stats_p95_sorts_samples_but_recent_tokens_keep_time_order() {
    let h = harness();
    seed_template_capability(&h.store, "cap.p95");
    // 时间升序采样，token 明显乱序：p95 必须先排序再取位，recent_tokens 仍按时间序。
    for (minutes, tokens) in [(40, 1), (30, 100), (20, 2), (10, 99)] {
        template_run(
            &h.audit,
            "cap.p95",
            minutes,
            "s-1",
            "ok",
            "miss",
            Some(tokens),
        );
    }
    let (status, body) = call(&h.app, Req::get("/admin/tools/stats")).await;
    assert_eq!(status, StatusCode::OK);
    let item = find_tool(&body, "cap.p95");
    assert_eq!(
        item["p95_response_tokens"],
        json!(100),
        "最近秩法 rank=ceil(0.95*4)=4，排序后第 4 小是 100"
    );
    assert_eq!(
        item["recent_tokens"],
        json!([1, 100, 2, 99]),
        "recent_tokens 保持时间升序，不因 p95 排序而改变"
    );
    assert_eq!(item["avg_response_tokens"], json!(50.5));
}

#[tokio::test]
async fn stats_lists_registered_capability_without_calls_as_unknown() {
    let h = harness();
    seed_capability(&h.store, "cap.idle");
    let (status, body) = call(&h.app, Req::get("/admin/tools/stats")).await;
    assert_eq!(status, StatusCode::OK);
    let idle = find_tool(&body, "cap.idle");
    assert_eq!(idle["calls"], json!(0));
    assert_eq!(idle["success"], json!(0));
    assert_eq!(idle["failure"], json!(0));
    assert_eq!(idle["success_rate"], Value::Null, "calls=0 时成功率是 null");
    assert_eq!(idle["hit_rate"], Value::Null);
    assert_eq!(idle["health"], json!("unknown"));
    assert_eq!(idle["retries"], json!(0));
    assert_eq!(idle["last_call_at"], Value::Null);
    assert_eq!(idle["last_outcome"], Value::Null);
    assert_eq!(idle["avg_response_tokens"], Value::Null);
    assert_eq!(idle["p95_response_tokens"], Value::Null);
    assert_eq!(idle["max_response_tokens"], Value::Null);
    assert_eq!(idle["recent_tokens"], json!([]));
}

#[tokio::test]
async fn stats_lists_all_builtin_ids() {
    let h = harness();
    let (status, body) = call(&h.app, Req::get("/admin/tools/stats")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(BUILTIN_CAPABILITY_IDS.len(), 7);
    for id in BUILTIN_CAPABILITY_IDS {
        let cap = find_tool(&body, id);
        assert_eq!(
            cap["source"],
            json!("builtin"),
            "{id} 的 source 必须是 builtin"
        );
        assert_eq!(cap["lifecycle_status"], json!("published"));
        assert_eq!(cap["health"], json!("unknown"));
    }
    let admin_only = find_tool(&body, "admin_register_template");
    assert_eq!(admin_only["group"], json!("admin"));
    let resident = find_tool(&body, "list_groups");
    assert_eq!(resident["group"], json!("basic"));

    // 三类职责的代表：basic 目录走查、analysis 列表分析与变更提交、admin 仅审计查询。
    for (id, category) in [
        ("search_capabilities", "basic"),
        ("describe_capability", "basic"),
        ("admin_review_queue", "analysis"),
        ("admin_audit_queries", "admin"),
    ] {
        assert_eq!(
            find_tool(&body, id)["category"],
            json!(category),
            "{id} 的 category"
        );
    }
}

#[tokio::test]
async fn stats_clamps_window_and_marks_limit_truncation() {
    let h = harness();
    seed_with(&h.store, "aaa.first", "db.read.core", "a");
    seed_with(&h.store, "bbb.second", "db.read.core", "b");
    // 窗口外的调用不得计入：60 天前远超 30 天上限（`minutes_ago` 以分钟为单位）。
    tool_call(&h.audit, 60 * 24 * 60, "s-old", "ok", Some(10));

    let (status, body) = call(&h.app, Req::get("/admin/tools/stats?window_days=0")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["window"]["days"], json!(1), "下界夹紧到 1");

    let (status, body) = call(&h.app, Req::get("/admin/tools/stats?window_days=999")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["window"]["days"], json!(30), "上界夹紧到 30");
    assert_eq!(
        find_tool(&body, "search_capabilities")["calls"],
        json!(0),
        "窗口外的调用不计入"
    );
    assert_eq!(body["truncated"], json!(false));

    let (status, body) = call(&h.app, Req::get("/admin/tools/stats?limit=1")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tools"].as_array().expect("tools").len(), 1);
    assert_eq!(body["truncated"], json!(true), "超过 limit 必须标记截断");
    assert_eq!(
        body["tools"][0]["id"],
        json!("aaa.first"),
        "calls 相同（0）时按 id 升序"
    );

    let (status, body) = call(&h.app, Req::get("/admin/tools/stats?limit=9999")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["limit"],
        Value::Null,
        "响应不带 limit（只有 window/truncated/tools）"
    );
    assert_eq!(
        body["tools"].as_array().expect("tools").len(),
        9,
        "7 个 builtin + 2 个注册能力"
    );
    assert_eq!(body["truncated"], json!(false));
}

#[tokio::test]
async fn tool_calls_detail_is_scoped_ordered_and_nullable() {
    let h = harness();
    seed_template_capability(&h.store, "order.daily_summary");
    template_run(
        &h.audit,
        "order.daily_summary",
        30,
        "s-1",
        "ok",
        "miss",
        Some(120),
    );
    template_run(
        &h.audit,
        "order.daily_summary",
        20,
        "s-1",
        "error",
        "miss",
        None,
    );
    tool_call(&h.audit, 10, "s-2", "ok", Some(100));

    let (status, body) = call(&h.app, Req::get("/admin/tools/order.daily_summary/calls")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], json!("order.daily_summary"));
    assert_eq!(body["total"], json!(2), "只含该 id 的事件");
    assert_eq!(body["limit"], json!(50));
    assert_eq!(body["offset"], json!(0));
    let calls = body["calls"].as_array().expect("calls");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["outcome"], json!("error"), "时间倒序");
    assert_eq!(calls[0]["tokens"], Value::Null, "缺失的 tokens 返回 null");
    assert_eq!(calls[0]["error_code"], Value::Null);
    assert_eq!(calls[0]["error_message"], Value::Null);
    assert_eq!(calls[0]["version"], json!(1));
    assert_eq!(calls[0]["cache"], json!("miss"));
    assert_eq!(calls[0]["elapsed_ms"], json!(5));
    assert_eq!(calls[0]["event_type"], json!("template_run"));
    assert_eq!(calls[0]["object_type"], json!("template"));
    assert_eq!(calls[0]["object_id"], json!("order.daily_summary"));
    assert_eq!(calls[0]["actor"]["id"], json!("a-1"));
    assert_eq!(calls[0]["actor"]["role"], json!("analyst"));
    assert_eq!(
        calls[0]["arguments"],
        json!({"template_id": "order.daily_summary", "params": {}})
    );
    assert_eq!(calls[0]["executed_script"], json!("SELECT 1"));
    assert_eq!(calls[0]["bound_params"], json!([]));
    assert!(calls[0]["event_id"].as_str().is_some());
    assert!(calls[0]["ts"].as_str().is_some());
    assert!(
        calls[0].get("session_id").is_none(),
        "session_id 不出现在明细"
    );
    assert!(calls[0].get("mcp_code").is_none(), "mcp_code 已删除");
    assert_eq!(calls[1]["tokens"], json!(120));

    // limit 夹紧 + offset 生效。
    let (status, body) = call(
        &h.app,
        Req::get("/admin/tools/order.daily_summary/calls?limit=9999&offset=1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["limit"], json!(200));
    assert_eq!(body["total"], json!(2));
    assert_eq!(body["calls"].as_array().expect("calls").len(), 1);
    assert_eq!(body["calls"][0]["tokens"], json!(120));

    // 未注册/无调用的 id：空列表而不是 404。
    let (status, body) = call(&h.app, Req::get("/admin/tools/nope/calls")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(0));
    assert_eq!(body["calls"], json!([]));
}

/// 明细只读结构化载荷：`data.arguments` / `data.executed_script` / `data.bound_params`。
/// 非结构化事件的 `sql`/`params`/`mcp_code` 一律不回退，这类事件在明细页不出现。
#[tokio::test]
async fn tool_calls_detail_reads_new_payload_only() {
    let h = harness();
    append_event(
        &h.audit,
        "tool_call",
        minutes_ago(20),
        "s-1",
        json!({
            "object_type": "tool",
            "object_id": "db_query_readonly",
            "action": "invoke",
            "outcome": "ok",
            "data": {
                "arguments": {"sql": "SELECT 2", "params": [2]},
                "executed_script": "SELECT 2",
                "bound_params": [2]
            }
        }),
    );
    // 旧载荷（只有 sql/params/mcp_code）：不参与明细。
    append_event(
        &h.audit,
        "tool_call",
        minutes_ago(15),
        "s-1",
        json!({
            "tool": "db_query_readonly",
            "outcome": "error",
            "sql": "SELECT 3",
            "params": [3],
            "mcp_code": "MCP_SQL_REJECTED"
        }),
    );
    // 新载荷的错误行：错误原因在顶层。
    append_event(
        &h.audit,
        "tool_call",
        minutes_ago(10),
        "s-1",
        json!({
            "object_type": "tool",
            "object_id": "db_query_readonly",
            "action": "invoke",
            "outcome": "error",
            "error_code": "MCP_SQL_REJECTED",
            "error_message": "只读查询被拒绝",
            "data": {
                "arguments": {"sql": "SELECT 4"},
                "bound_params": []
            }
        }),
    );

    let (status, body) = call(&h.app, Req::get("/admin/tools/db_query_readonly/calls")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(2), "旧载荷行不计入明细");
    let calls = body["calls"].as_array().expect("calls");
    assert_eq!(calls[0]["error_code"], json!("MCP_SQL_REJECTED"));
    assert_eq!(calls[0]["error_message"], json!("只读查询被拒绝"));
    assert_eq!(calls[0]["arguments"], json!({"sql": "SELECT 4"}));
    assert_eq!(calls[0]["executed_script"], Value::Null, "缺失时不回退 sql");
    assert_eq!(calls[0]["bound_params"], json!([]));
    assert_eq!(
        calls[1]["arguments"],
        json!({"sql": "SELECT 2", "params": [2]})
    );
    assert_eq!(calls[1]["executed_script"], json!("SELECT 2"));
    assert_eq!(calls[1]["bound_params"], json!([2]));
    assert_eq!(calls[1]["outcome"], json!("ok"));
}

#[tokio::test]
async fn stats_retries_are_counted_per_session_not_globally() {
    let h = harness();
    seed_template_capability(&h.store, "cap.interleaved");
    seed_template_capability(&h.store, "cap.consecutive");
    // A 失败 → B 调用 → A 重试：其它会话插在中间不得吞掉 A 的重试（全局 windows(2) 会漏计）。
    template_run(
        &h.audit,
        "cap.interleaved",
        50,
        "s-a",
        "error",
        "miss",
        None,
    );
    template_run(
        &h.audit,
        "cap.interleaved",
        45,
        "s-b",
        "ok",
        "miss",
        Some(10),
    );
    template_run(
        &h.audit,
        "cap.interleaved",
        40,
        "s-a",
        "ok",
        "hit",
        Some(20),
    );
    // 同一会话连续两次失败再成功：每次都紧跟下一次 → 2 次重试。
    template_run(
        &h.audit,
        "cap.consecutive",
        30,
        "s-a",
        "error",
        "miss",
        None,
    );
    template_run(
        &h.audit,
        "cap.consecutive",
        25,
        "s-a",
        "unknown",
        "miss",
        None,
    );
    template_run(
        &h.audit,
        "cap.consecutive",
        20,
        "s-a",
        "ok",
        "miss",
        Some(5),
    );

    let (status, body) = call(&h.app, Req::get("/admin/tools/stats")).await;
    assert_eq!(status, StatusCode::OK);
    let interleaved = find_tool(&body, "cap.interleaved");
    assert_eq!(interleaved["calls"], json!(3));
    assert_eq!(interleaved["success"], json!(2));
    assert_eq!(interleaved["failure"], json!(1));
    assert_eq!(
        interleaved["retries"],
        json!(1),
        "s-a 的失败后同一会话还有一次尝试 → 计 1 次重试"
    );

    let consecutive = find_tool(&body, "cap.consecutive");
    assert_eq!(consecutive["calls"], json!(3));
    assert_eq!(
        consecutive["retries"],
        json!(2),
        "两次失败后都跟了下一次调用"
    );
}

#[tokio::test]
async fn tool_calls_window_defaults_to_thirty_days_and_clamps() {
    let h = harness();
    seed_template_capability(&h.store, "order.history");
    template_run(
        &h.audit,
        "order.history",
        10,
        "s-new",
        "ok",
        "miss",
        Some(10),
    );
    // 60 天前：默认 30 天窗口之外、90 天窗口之内（审计 append 保留显式 ts）。
    template_run(
        &h.audit,
        "order.history",
        60 * 24 * 60,
        "s-old",
        "ok",
        "miss",
        Some(99),
    );

    let (status, body) = call(&h.app, Req::get("/admin/tools/order.history/calls")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(1), "默认 30 天窗口不含 60 天前的调用");
    assert_eq!(body["calls"][0]["object_id"], json!("order.history"));
    assert_eq!(body["calls"][0]["tokens"], json!(10));
    assert!(body["calls"][0].get("session_id").is_none());

    let (status, body) = call(
        &h.app,
        Req::get("/admin/tools/order.history/calls?window_days=90"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(2), "90 天窗口包含 60 天前的调用");
    assert_eq!(body["calls"][1]["tokens"], json!(99));

    // window_days 夹紧到 1：只保留最近的调用，分页字段照常返回。
    let (status, body) = call(
        &h.app,
        Req::get("/admin/tools/order.history/calls?window_days=0"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["limit"], json!(50));
    assert_eq!(body["offset"], json!(0));

    // 上界夹紧到 90 天：9999 与 90 等价。
    let (status, body) = call(
        &h.app,
        Req::get("/admin/tools/order.history/calls?window_days=9999"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(2));

    // 未知查询键仍然 400。
    let (status, body) = call(&h.app, Req::get("/admin/tools/order.history/calls?bogus=1")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_FILTER_UNSUPPORTED"));
}

/// 统计口径：`surface="admin"` 的事件一律不计入工作台/排行，但审计查询照常返回。
///
/// 为什么这条必须钉住：管理台每次打开看板都会写 `tool_call`/`admin_change`，
/// 一旦统计把平台面算进去，看板就会被自己的刷新刷量——数字越大越"活跃"，
/// 而真正的问题是没人用 MCP。审计可查保证了排除不等于隐藏。
#[tokio::test]
async fn admin_surface_events_are_excluded_from_stats_but_queryable() {
    let h = harness();
    seed_template_capability(&h.store, "order.daily_summary");
    template_run(
        &h.audit,
        "order.daily_summary",
        30,
        "s-1",
        "ok",
        "miss",
        Some(100),
    );
    // 同名工具在平台面被"调用"：业务面 1 次 + 平台面 1 次，统计只应看到 1 次。
    append_admin_event(
        &h.audit,
        "tool_call",
        10,
        "s-admin",
        json!({
            "object_type": "tool",
            "object_id": "search_capabilities",
            "action": "invoke",
            "outcome": "ok",
            "data": {"elapsed_ms": 1}
        }),
    );
    append_admin_event(
        &h.audit,
        "tool_call",
        10,
        "s-admin",
        json!({
            "object_type": "template",
            "object_id": "order.daily_summary",
            "action": "invoke",
            "outcome": "error",
            "data": {"elapsed_ms": 1}
        }),
    );

    let (status, body) = call(&h.app, Req::get("/admin/tools/stats")).await;
    assert_eq!(status, StatusCode::OK);
    let tpl = find_tool(&body, "order.daily_summary");
    assert_eq!(tpl["calls"], json!(1), "平台面的 template_run 不计入");
    assert_eq!(tpl["failure"], json!(0), "平台面的失败也不计入失败榜");
    // builtin 工具始终在台账里（元信息来自代码），但调用次数必须为 0。
    let search = find_tool(&body, "search_capabilities");
    assert_eq!(search["calls"], json!(0), "平台面的 tool_call 不计入");

    // 明细同样排除：否则点进工具详情又能看见被统计口径排除的调用。
    let (status, body) = call(&h.app, Req::get("/admin/tools/search_capabilities/calls")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(0));

    // 审计查询仍然返回它们，并且 surface 过滤生效。
    let (status, body) = call(&h.app, Req::get("/admin/audit-events?surface=admin")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(2));
    let events = body["events"].as_array().expect("events");
    assert!(events.iter().all(|e| e["surface"] == json!("admin")));

    let (status, body) = call(&h.app, Req::get("/admin/audit-events?surface=mcp")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(1), "业务面只有那条 template_run");
    let events = body["events"].as_array().expect("events");
    assert!(events.iter().all(|e| e["surface"] == json!("mcp")));
}
