use crate::support::*;
use axum::http::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn groups_crud_takes_effect_in_effective_policy() {
    let h = harness();
    let (status, body) = call(&h.app, Req::get("/admin/groups")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(2), "内置 basic + 一个 policy 组");
    assert_eq!(
        body["items"][0]["code"],
        json!("basic"),
        "内置组固定排在第一位"
    );
    assert_eq!(body["items"][0]["source"], json!("builtin"));
    assert_eq!(body["items"][0]["name"], json!("基础工具"));
    assert_eq!(body["items"][0]["priority"], json!(0));
    assert_eq!(body["items"][0]["min_budget"], json!(800));
    assert_eq!(body["items"][0]["roles"], json!([]));
    assert_eq!(
        body["items"][0]["grantable"],
        json!(true),
        "basic 是可授权的真实组：常驻元工具靠它放行"
    );
    assert_eq!(body["items"][0]["token_count"], json!(0));
    assert_eq!(
        body["items"][0]["capability_count"],
        json!(4),
        "没有上游覆盖时 basic 含 4 个常驻内置工具"
    );
    let policy_item = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["code"] == json!("db.read.core"))
        .expect("policy group");
    assert_eq!(
        policy_item["name"],
        json!("核心数据只读"),
        "组条目必须同时给 code 与中文展示名"
    );
    assert_eq!(policy_item["source"], json!("policy"));
    assert_eq!(policy_item["grantable"], json!(true));
    assert_eq!(
        policy_item["roles"],
        json!(["analyst"]),
        "policy 组角色从 roles 反查（角色 code）"
    );

    let (status, body) = call(
        &h.app,
        Req::post("/admin/groups").body(json!({
            "code": "db.read.new",
            "name": "新只读组",
            "priority": 7,
            "min_budget": 900,
            "roles": ["analyst"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["code"], json!("db.read.new"));
    assert_eq!(body["name"], json!("新只读组"));
    assert_eq!(body["source"], json!("managed"));
    assert_eq!(body["roles"], json!(["analyst"]));
    assert_eq!(body["capability_count"], json!(0));

    // 重复（managed）→ 409。
    let (status, body) = call(
        &h.app,
        Req::post("/admin/groups").body(json!({
            "code": "db.read.new", "name": "重复组", "priority": 1, "min_budget": 800, "roles": []
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_GROUP_CONFLICT"));

    // 未知角色 → 400。
    let (status, body) = call(
        &h.app,
        Req::post("/admin/groups").body(json!({
            "code": "db.read.ghost", "name": "幽灵组", "priority": 1, "min_budget": 800,
            "roles": ["ghost"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_VALIDATION_ERROR"));
    assert_eq!(body["error"]["data"]["unknown_roles"], json!(["ghost"]));

    // 修改：name/priority/min_budget/roles 可改，code 不可改。
    let (status, body) = call(
        &h.app,
        Req::put("/admin/groups/db.read.new").body(json!({
            "name": "新只读组（改名）", "priority": 9, "min_budget": 1000, "roles": ["analyst"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], json!("db.read.new"), "路径 code 保持不变");
    assert_eq!(body["name"], json!("新只读组（改名）"));
    assert_eq!(body["priority"], json!(9));
    assert_eq!(body["min_budget"], json!(1000));

    // body 里出现 code → 400（拒绝静默忽略改名）。
    let (status, body) = call(
        &h.app,
        Req::put("/admin/groups/db.read.new")
            .body(json!({"code": "db.read.other", "name": "换 code"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_VALIDATION_ERROR"));
    assert_eq!(body["error"]["data"]["field"], json!("code"));

    // 有效策略立刻生效：新组按 code 进入 groups（展示名一并保留），并追加进 analyst 默认组。
    let policy = h.service.effective_policy().expect("effective policy");
    let merged = policy
        .groups
        .iter()
        .find(|g| g.code == "db.read.new")
        .expect("managed group merged by code");
    assert_eq!(merged.name, "新只读组（改名）");
    assert!(
        policy.roles["analyst"]
            .default_groups
            .contains(&"db.read.new".to_string())
    );

    // 列表合并 policy + managed（外加内置 basic）。
    let (status, body) = call(&h.app, Req::get("/admin/groups")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(3));
    let managed = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["code"] == json!("db.read.new"))
        .expect("managed group");
    assert_eq!(managed["name"], json!("新只读组（改名）"));
    assert_eq!(managed["source"], json!("managed"));
    assert_eq!(managed["priority"], json!(9));

    // 删除 → 下一次 effective policy 不包含它；响应带 code（name 仅是展示）。
    let (status, body) = call(&h.app, Req::delete("/admin/groups/db.read.new")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["deleted"], json!(true));
    assert_eq!(body["code"], json!("db.read.new"));
    assert_eq!(body["name"], json!("新只读组（改名）"));
    let policy = h.service.effective_policy().expect("effective policy");
    assert!(!policy.groups.iter().any(|g| g.code == "db.read.new"));

    // 三次写操作都落 admin_change：object_type=group，object_id=code（无 `group:` 前缀）。
    let changes = h
        .audit
        .query(Some(&json!({"event_type": "admin_change"})))
        .expect("query");
    let group_changes: Vec<_> = changes
        .iter()
        .filter(|e| e.payload["object_type"] == json!("group"))
        .collect();
    assert_eq!(group_changes.len(), 3, "create/update/delete 各一条");
    assert_eq!(group_changes[0].payload["object_id"], json!("db.read.new"));
    assert_eq!(group_changes[0].payload["action"], json!("group_create"));
    assert_eq!(group_changes[1].payload["action"], json!("group_update"));
    assert_eq!(group_changes[2].payload["action"], json!("group_delete"));
    for change in &group_changes {
        assert_eq!(change.payload["approval"], Value::Null);
        assert!(
            change.payload["changes"]
                .as_array()
                .is_some_and(|c| !c.is_empty())
        );
        assert_eq!(
            change.payload["data"]["group"]["code"],
            json!("db.read.new")
        );
        assert!(
            change.payload["data"]["group"]["name"]
                .as_str()
                .is_some_and(|n| !n.is_empty()),
            "组审计 data 必须带展示名: {change:?}"
        );
    }
    // 改名落在 changes 的 name 字段上（before/after 都是展示名）。
    let renamed = group_changes[1].payload["changes"]
        .as_array()
        .expect("changes")
        .iter()
        .find(|c| c["field"] == json!("name"))
        .expect("update 审计含 name 变化");
    assert_eq!(renamed["before"], json!("新只读组"));
    assert_eq!(renamed["after"], json!("新只读组（改名）"));
}

#[tokio::test]
async fn groups_list_filters_by_q_source_and_role() {
    let h = harness();
    // policy 组 db.read.core（核心数据只读，analyst）+ 两个管理组。
    call(
        &h.app,
        Req::post("/admin/groups").body(json!({
            "code": "db.read.analytics2", "name": "分析快照组", "priority": 3,
            "min_budget": 800, "roles": ["analyst"]
        })),
    )
    .await;
    call(
        &h.app,
        Req::post("/admin/groups").body(json!({
            "code": "kb.extra", "name": "补充知识库", "priority": 2,
            "min_budget": 800, "roles": []
        })),
    )
    .await;

    // 不带过滤：全部 4 条（内置 basic + policy + 两个管理组）。
    let (_, body) = call(&h.app, Req::get("/admin/groups")).await;
    assert_eq!(body["total"], json!(4));

    // q 对 code 做小写包含。
    let (_, body) = call(&h.app, Req::get("/admin/groups?q=ANALYTICS")).await;
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["items"][0]["code"], json!("db.read.analytics2"));

    // q 对中文展示名做包含。
    let (_, body) = call(&h.app, Req::get("/admin/groups?q=知识库")).await;
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["items"][0]["code"], json!("kb.extra"));

    // source 过滤。
    let (_, body) = call(&h.app, Req::get("/admin/groups?source=policy")).await;
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["items"][0]["code"], json!("db.read.core"));
    let (_, body) = call(&h.app, Req::get("/admin/groups?source=managed")).await;
    assert_eq!(body["total"], json!(2));
    let (_, body) = call(&h.app, Req::get("/admin/groups?source=builtin")).await;
    assert_eq!(body["total"], json!(1), "内置组只有一个 basic");
    assert_eq!(body["items"][0]["code"], json!("basic"));
    assert_eq!(
        body["items"][0]["grantable"],
        json!(true),
        "basic 可授予：常驻元工具靠它放行"
    );

    // role 过滤：角色 code 精确匹配授权角色。
    let (_, body) = call(&h.app, Req::get("/admin/groups?role=analyst")).await;
    assert_eq!(body["total"], json!(2), "只返回授权给 analyst 的组");
    let codes: Vec<&str> = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|i| i["code"].as_str())
        .collect();
    assert_eq!(codes, vec!["db.read.core", "db.read.analytics2"]);

    // 组合过滤。
    let (_, body) = call(
        &h.app,
        Req::get("/admin/groups?source=managed&role=analyst&q=快照"),
    )
    .await;
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["items"][0]["code"], json!("db.read.analytics2"));

    // 非法 source → 400。
    let (status, body) = call(&h.app, Req::get("/admin/groups?source=config")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("MCP_VALIDATION_ERROR"));
}

#[tokio::test]
async fn roles_list_returns_code_and_chinese_name() {
    let h = harness();
    let (status, body) = call(&h.app, Req::get("/admin/roles")).await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["code"], json!("analyst"));
    assert_eq!(items[0]["name"], json!("数据分析"));
    assert_eq!(items[0]["default_groups"], json!(["basic", "db.read.core"]));
    assert_eq!(items[0]["default_budget"], json!(2000));
}

#[tokio::test]
async fn groups_reject_reserved_policy_and_missing() {
    let h = harness();
    for code in ["basic", "builtin", "admin"] {
        let (status, body) = call(
            &h.app,
            Req::post("/admin/groups").body(json!({
                "code": code, "name": "保留组", "priority": 1, "min_budget": 800, "roles": []
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "创建保留组 {code}");
        assert_eq!(body["error"]["code"], json!("MCP_GROUP_RESERVED"));

        let (status, body) = call(
            &h.app,
            Req::put(&format!("/admin/groups/{code}"))
                .body(json!({"priority": 1, "min_budget": 800, "roles": []})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "修改保留组 {code}");
        assert_eq!(body["error"]["code"], json!("MCP_GROUP_RESERVED"));

        let (status, body) = call(&h.app, Req::delete(&format!("/admin/groups/{code}"))).await;
        assert_eq!(status, StatusCode::CONFLICT, "删除保留组 {code}");
        assert_eq!(body["error"]["code"], json!("MCP_GROUP_RESERVED"));
    }

    // 配置组：创建/修改/删除一律 409（只能改配置文件）。
    let (status, body) = call(
        &h.app,
        Req::post("/admin/groups").body(json!({
            "code": "db.read.core", "name": "核心数据只读", "priority": 1,
            "min_budget": 800, "roles": ["analyst"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_GROUP_CONFLICT"));
    let (status, body) = call(
        &h.app,
        Req::put("/admin/groups/db.read.core")
            .body(json!({"priority": 1, "min_budget": 800, "roles": ["analyst"]})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_GROUP_CONFLICT"));
    let (status, body) = call(&h.app, Req::delete("/admin/groups/db.read.core")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_GROUP_CONFLICT"));

    // 不存在 → 404（改/删）。
    let (status, body) = call(
        &h.app,
        Req::put("/admin/groups/db.read.missing")
            .body(json!({"priority": 1, "min_budget": 800, "roles": []})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], json!("MCP_GROUP_NOT_FOUND"));
    let (status, body) = call(&h.app, Req::delete("/admin/groups/db.read.missing")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], json!("MCP_GROUP_NOT_FOUND"));

    // 非法组 code / 空展示名 / 非法数值 → 400。
    for body in [
        json!({"code": "Bad Name", "name": "大写组", "priority": 1, "min_budget": 800, "roles": []}),
        json!({"code": "db.read.bad", "name": "   ", "priority": 1, "min_budget": 800, "roles": []}),
        json!({"code": "db.read.bad", "name": "组", "priority": 1, "min_budget": 0, "roles": []}),
        json!({"code": "db.read.bad", "name": "组", "priority": -1, "min_budget": 800, "roles": []}),
    ] {
        let (status, resp) = call(&h.app, Req::post("/admin/groups").body(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(resp["error"]["code"], json!("MCP_VALIDATION_ERROR"));
    }
}

#[tokio::test]
async fn group_delete_refused_while_capability_uses_it() {
    let h = harness();
    call(
        &h.app,
        Req::post("/admin/groups").body(json!({
            "code": "db.read.used", "name": "被引用组", "priority": 1,
            "min_budget": 800, "roles": ["analyst"]
        })),
    )
    .await;
    h.store
        .register(
            "cap.used",
            "tool",
            "db.read.used",
            "read",
            json!({"description": "引用组"}),
        )
        .expect("register");

    let (status, body) = call(&h.app, Req::delete("/admin/groups/db.read.used")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("MCP_GROUP_IN_USE"));
    assert_eq!(body["error"]["data"]["capability_count"], json!(1));

    // 列表上 capability_count 同步。
    let (_, body) = call(&h.app, Req::get("/admin/groups")).await;
    let used = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["code"] == json!("db.read.used"))
        .expect("group");
    assert_eq!(used["capability_count"], json!(1));
}
