use super::*;

use serde_json::{Value, json};

fn store() -> CapabilityStore {
    CapabilityStore::open(":memory:", None).unwrap()
}
/// 内置 id 必须严格分区：BUILTIN = RESIDENT ∪ ADMIN，且两个子集互不重叠。
/// 只在其中一处加 id 会让"写保护"或"发现面投影"单边失效。
#[test]
fn builtin_ids_partition_matches_subsets() {
    assert_eq!(
        RESIDENT_CAPABILITY_IDS.len() + ADMIN_CAPABILITY_IDS.len(),
        BUILTIN_CAPABILITY_IDS.len()
    );
    for id in RESIDENT_CAPABILITY_IDS.iter().chain(ADMIN_CAPABILITY_IDS) {
        assert!(BUILTIN_CAPABILITY_IDS.contains(id), "{id} 不在写保护集合");
        assert!(is_builtin_id(id), "{id} 未被 is_builtin_id 识别");
    }
    for id in RESIDENT_CAPABILITY_IDS {
        assert!(
            !ADMIN_CAPABILITY_IDS.contains(id),
            "{id} 同时属于常驻与维护"
        );
    }
}
#[test]
fn write_and_admin_risks_are_not_registerable() {
    let s = store();
    for risk in ["write", "admin"] {
        let e = s
            .register("cap1", "tool", "db.read.core", risk, json!({}))
            .unwrap_err();
        assert_eq!(e.code, "MCP_VALIDATION_ERROR");
    }
}
#[test]
fn self_approval_publishes() {
    let s = store();
    s.register(
        "cap1",
        "tool",
        "db.read.core",
        "read",
        json!({"description":"x"}),
    )
    .unwrap();
    s.submit_for_review("cap1", "alice").unwrap();
    let r = s.promote_to_published("cap1", "ap-1", "alice").unwrap();
    assert_eq!(r.status, "published");
    let v1 = s.versions("cap1").unwrap().pop().unwrap();
    assert_eq!(v1.submitted_by, "alice");
    assert_eq!(v1.approved_by, "alice");
    assert_eq!(v1.approval_id, "ap-1");
    assert_eq!(v1.status, "published");
}
#[test]
fn new_version_not_inplace() {
    let s = store();
    s.register(
        "cap1",
        "tool",
        "db.read.core",
        "read",
        json!({"description":"v1"}),
    )
    .unwrap();
    let r = s
        .publish_version("cap1", json!({"description":"v2"}), "ap-1", "alice", "bob")
        .unwrap();
    assert_eq!(r.version, 2);
    assert_eq!(
        s.get_version("cap1", 1)
            .unwrap()
            .unwrap()
            .definition_hash
            .len(),
        64
    );
}
#[test]
fn deprecate_needs_successor() {
    let s = store();
    s.register("cap1", "tool", "db.read.core", "read", json!({}))
        .unwrap();
    assert!(s.deprecate("cap1", "").is_err());
    let r = s.deprecate("cap1", "cap2").unwrap();
    assert_eq!(r.status, "deprecated");
}
#[test]
fn versions_append_only_trigger_blocks_update() {
    let s = store();
    s.register(
        "cap1",
        "tool",
        "db.read.core",
        "read",
        json!({"description":"v1"}),
    )
    .unwrap();
    let conn = s.conn.lock().unwrap();
    assert!(
        conn.execute(
            "UPDATE capability_versions SET definition_json='x' WHERE capability_id='cap1'",
            []
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "DELETE FROM capability_versions WHERE capability_id='cap1'",
            []
        )
        .is_err()
    );
}
#[test]
fn publish_version_old_unchanged_pointer_moves_audit_differs() {
    let audit = AuditLog::open(":memory:").unwrap();
    let s = CapabilityStore::open(":memory:", Some(std::sync::Arc::new(audit))).unwrap();
    s.register(
        "cap1",
        "tool",
        "db.read.core",
        "read",
        json!({"description":"v1","keep":1}),
    )
    .unwrap();
    s.submit_for_review("cap1", "alice").unwrap();
    s.promote_to_published("cap1", "ap-1", "bob").unwrap();
    let before = s.get("cap1").unwrap().unwrap();
    let before_hash = before.definition_hash.clone();
    let r = s
        .publish_version(
            "cap1",
            json!({"description":"v2","keep":1,"new":2}),
            "ap-2",
            "alice",
            "bob",
        )
        .unwrap();
    assert_eq!(r.version, 2);
    assert_eq!(r.status, "published");
    assert_ne!(r.definition_hash, before_hash);
    let v1 = s.get_version("cap1", 1).unwrap().unwrap();
    assert_eq!(v1.definition_hash, before_hash);
    assert_eq!(v1.definition["description"], json!("v1"));
    // 审计：capability/update_definition，changes 给出真实前后值，审批引用是外部票号。
    let rows = {
        let conn = s.audit.as_ref().unwrap();
        conn.query(None).unwrap()
    };
    let admin: Vec<_> = rows
        .into_iter()
        .filter(|e| e.event_type == "admin_change")
        .collect();
    assert!(!admin.is_empty());
    let publish = admin
        .iter()
        .find(|e| e.payload["action"] == json!("publish"))
        .expect("publish 事件");
    assert_eq!(publish.actor["id"], json!("bob"), "发布 actor 必须是批准人");
    let last = admin.last().unwrap();
    assert_eq!(last.payload["object_type"], json!("capability"));
    assert_eq!(last.payload["object_id"], json!("cap1"));
    assert_eq!(last.payload["action"], json!("update_definition"));
    assert_eq!(last.payload["outcome"], json!("ok"));
    assert_eq!(
        last.payload["data"]["definition_hash_before"],
        json!(before_hash)
    );
    assert_eq!(
        last.payload["data"]["definition_hash_after"],
        json!(r.definition_hash)
    );
    assert_ne!(
        last.payload["data"]["definition_hash_before"],
        last.payload["data"]["definition_hash_after"]
    );
    let changed: Vec<&str> = last.payload["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["field"].as_str())
        .collect();
    assert!(
        changed.contains(&"new"),
        "changes 应含新增 key: {changed:?}"
    );
    assert!(
        changed.contains(&"description"),
        "changes 应含修改 key: {changed:?}"
    );
    assert_eq!(last.payload["approval"]["id"], json!("ap-2"));
    assert_eq!(last.payload["approval"]["source"], json!("external"));
    assert_eq!(last.payload["approval"]["verified"], json!(false));
    // capabilities 指针已指向新 version
    assert_eq!(s.get("cap1").unwrap().unwrap().version, 2);
}
#[test]
fn publish_version_allows_same_person() {
    let s = store();
    s.register(
        "cap1",
        "tool",
        "db.read.core",
        "read",
        json!({"description":"v1"}),
    )
    .unwrap();
    let r = s
        .publish_version(
            "cap1",
            json!({"description":"v2"}),
            "ap-1",
            "alice",
            "alice",
        )
        .unwrap();
    assert_eq!(r.version, 2);
    let v2 = s.versions("cap1").unwrap().pop().unwrap();
    assert_eq!(v2.submitted_by, "alice");
    assert_eq!(v2.approved_by, "alice");
    assert_eq!(v2.approval_id, "ap-1");
    assert_eq!(v2.status, "published");
}
#[test]
fn publish_version_transaction_rolls_back() {
    let s = store();
    s.register(
        "cap1",
        "tool",
        "db.read.core",
        "read",
        json!({"description":"v1"}),
    )
    .unwrap();
    s.submit_for_review("cap1", "alice").unwrap();
    s.promote_to_published("cap1", "ap-1", "bob").unwrap();
    let before = s.get("cap1").unwrap().unwrap();
    // 注入必失败约束：后开一个 trigger 让第二次插入冲突
    {
        let conn = s.conn.lock().unwrap();
        // 人为制造单次失败：插一行会导致 pointer 更新触发失败（用不合法的 version 重复）
        // 替代方案：在新事务里插入 versions 后 pointer 更新失败会回滚；这里用 MUST_FAIL 的 SQL 模拟
        let _ = conn.execute("CREATE TEMP TRIGGER trg_fail AFTER INSERT ON capability_versions BEGIN SELECT RAISE(ABORT, 'forced'); END;", []);
    }
    let res = s.publish_version("cap1", json!({"description":"v2"}), "ap-2", "alice", "bob");
    assert!(res.is_err());
    let after = s.get("cap1").unwrap().unwrap();
    assert_eq!(after.version, before.version);
    assert_eq!(after.definition_hash, before.definition_hash);
    assert!(s.get_version("cap1", 2).unwrap().is_none());
}
#[test]
fn db_failure_get_visible_all_returns_err() {
    let s = store();
    s.register(
        "cap1",
        "tool",
        "db.read.core",
        "read",
        json!({"description":"v1"}),
    )
    .unwrap();
    {
        let conn = s.conn.lock().unwrap();
        let _ = conn.execute("DROP TABLE capabilities", []);
    }
    assert!(s.get("cap1").is_err());
    assert!(s.all().is_err());
    assert!(s.visible_ids(&["db.read.core".to_string()]).is_err());
}
#[test]
fn empty_table_is_ok_empty() {
    let s = store();
    assert!(s.get("missing").unwrap().is_none());
    assert_eq!(s.all().unwrap().len(), 0);
    assert_eq!(
        s.visible_ids(&["db.read.core".to_string()]).unwrap().len(),
        0
    );
    assert_eq!(s.visible_ids(&[]).unwrap().len(), 0);
}
#[test]
fn publish_version_conflict_keeps_pointer() {
    let s = store();
    s.register(
        "cap1",
        "tool",
        "db.read.core",
        "read",
        json!({"description":"v1"}),
    )
    .unwrap();
    // 预置 version 2 行：模拟并发/外部写入造成的版本占用。
    {
        let conn = s.conn.lock().unwrap();
        conn.execute(
                "INSERT INTO capability_versions (capability_id, version, definition_json, definition_hash, approval_id) VALUES ('cap1', 2, '{}', 'x', 'ap-0')",
                [],
            )
            .unwrap();
    }
    let e = s
        .publish_version("cap1", json!({"description":"v2"}), "ap-1", "alice", "bob")
        .unwrap_err();
    assert_eq!(e.code, "MCP_VERSION_CONFLICT");
    // 事务回滚：指针不前进、定义不变。
    let rec = s.get("cap1").unwrap().unwrap();
    assert_eq!(rec.version, 1);
    assert_eq!(rec.definition["description"], json!("v1"));
}

/// 内置 id 在读侧由代码提供，写侧必须一律被拒：包括 register 自己——
/// 否则维护通道可以"注册"一条同名能力，在发现面影子覆盖内置工具定义。
#[test]
fn builtin_ids_are_immutable_across_every_write_path() {
    let s = store();
    for id in BUILTIN_CAPABILITY_IDS {
        let errors = [
            s.register(
                id,
                "tool",
                "db.read.core",
                "read",
                json!({"description":"影子"}),
            )
            .unwrap_err(),
            s.submit_for_review(id, "alice").unwrap_err(),
            s.reject_review(id, "理由").unwrap_err(),
            s.promote_to_published(id, "ap-1", "bob").unwrap_err(),
            s.publish_version(id, json!({"description":"v2"}), "ap-2", "alice", "bob")
                .unwrap_err(),
            s.deprecate(id, "successor").unwrap_err(),
            s.set_visibility(id, Some(true), Some(true), "alice")
                .unwrap_err(),
        ];
        for e in errors {
            assert_eq!(e.code, "MCP_BUILTIN_IMMUTABLE", "{id} 的写操作必须被拒");
            assert_eq!(e.data["capability_id"], json!(id));
        }
        // 拒绝发生在落库之前：注册表里不该出现这一行。
        assert!(s.get(id).unwrap().is_none(), "{id} 不得被写入注册表");
    }
}

#[test]
fn visible_filters_by_status_discoverable_and_group() {
    let s = store();
    s.register(
        "cap.draft",
        "tool",
        "db.read.core",
        "read",
        json!({"description":"draft"}),
    )
    .unwrap();
    s.register(
        "cap.other_group",
        "tool",
        "db.admin.schema",
        "read",
        json!({"description":"other"}),
    )
    .unwrap();
    s.submit_for_review("cap.other_group", "alice").unwrap();
    s.promote_to_published("cap.other_group", "ap-1", "bob")
        .unwrap();
    s.set_visibility("cap.other_group", Some(true), Some(true), "alice")
        .unwrap();
    s.register(
        "cap.hidden",
        "tool",
        "db.read.core",
        "read",
        json!({"description":"hidden"}),
    )
    .unwrap();
    s.submit_for_review("cap.hidden", "alice").unwrap();
    s.promote_to_published("cap.hidden", "ap-3", "bob").unwrap();

    let groups = vec!["db.read.core".to_string()];
    let ids: Vec<String> = s
        .visible(&groups)
        .unwrap()
        .into_iter()
        .map(|r| r.capability_id)
        .collect();
    // draft 不可见；他组不可见；published 但 discoverable=false 不可见。
    assert!(ids.is_empty(), "可见集应为空，实际 {ids:?}");
    assert_eq!(s.visible_ids(&groups).unwrap(), ids);

    // discoverable=false + callable=true 落在调用面，不在发现面。
    s.set_visibility("cap.hidden", Some(false), Some(true), "alice")
        .unwrap();
    assert!(
        s.visible(&groups).unwrap().is_empty(),
        "不可发现的能力不得进入可见集"
    );
    let callable: Vec<String> = s
        .callable(&groups)
        .unwrap()
        .into_iter()
        .map(|r| r.capability_id)
        .collect();
    assert_eq!(
        callable,
        vec!["cap.hidden".to_string()],
        "可调用集只看 published ∧ callable ∧ group"
    );
}

fn managed(code: &str, priority: i64, min_budget: i64, roles: &[&str]) -> ManagedGroup {
    ManagedGroup {
        code: code.into(),
        name: format!("{code} 展示名"),
        priority,
        min_budget,
        roles: roles.iter().map(|r| (*r).to_string()).collect(),
        updated_at: String::new(),
    }
}

#[test]
fn managed_group_crud_conflicts_and_reserved_names() {
    let s = store();
    let created = s
        .insert_managed_group(&managed("db.read.new", 7, 900, &["analyst"]), "alice")
        .unwrap();
    assert_eq!(created.code, "db.read.new");
    assert_eq!(created.name, "db.read.new 展示名");
    assert_eq!(created.roles, vec!["analyst".to_string()]);
    assert_eq!(s.managed_groups().unwrap().len(), 1);

    // 重名 → 409 语义的错误码。
    let e = s
        .insert_managed_group(&managed("db.read.new", 1, 800, &[]), "alice")
        .unwrap_err();
    assert_eq!(e.code, "MCP_GROUP_CONFLICT");

    // 修改（不存在 → 404）。
    let updated = s
        .update_managed_group(&managed("db.read.new", 3, 1200, &["analyst"]), "bob")
        .unwrap();
    assert_eq!(updated.priority, 3);
    assert_eq!(updated.min_budget, 1200);
    let e = s
        .update_managed_group(&managed("db.read.missing", 3, 1200, &[]), "bob")
        .unwrap_err();
    assert_eq!(e.code, "MCP_GROUP_NOT_FOUND");

    // 字段门禁：min_budget<=0 / priority<0 / 非法组名 → 校验错误。
    for bad in [
        managed("db.read.bad", 1, 0, &[]),
        managed("db.read.bad", -1, 800, &[]),
        ManagedGroup {
            code: "db read bad".into(),
            ..managed("x", 1, 800, &[])
        },
    ] {
        assert_eq!(bad.validate().unwrap_err().code, "MCP_VALIDATION_ERROR");
    }

    // 保留组名：三个写路径都必须拒绝（admin 是维护通道安全边界）。
    for name in ["builtin", "admin"] {
        assert_eq!(
            s.insert_managed_group(&managed(name, 1, 800, &[]), "alice")
                .unwrap_err()
                .code,
            "MCP_GROUP_RESERVED"
        );
        assert_eq!(
            s.update_managed_group(&managed(name, 1, 800, &[]), "alice")
                .unwrap_err()
                .code,
            "MCP_GROUP_RESERVED"
        );
        assert_eq!(
            s.remove_managed_group(name, "alice").unwrap_err().code,
            "MCP_GROUP_RESERVED"
        );
    }

    // 引用中的组不可删除，错误里带精确计数。
    s.register(
        "cap1",
        "tool",
        "db.read.new",
        "read",
        json!({"description":"x"}),
    )
    .unwrap();
    assert_eq!(s.group_capability_count("db.read.new").unwrap(), 1);
    let e = s.remove_managed_group("db.read.new", "alice").unwrap_err();
    assert_eq!(e.code, "MCP_GROUP_IN_USE");
    assert_eq!(e.data["capability_count"], json!(1));

    // 无引用的组可以删除。
    s.insert_managed_group(&managed("db.read.empty", 1, 800, &[]), "alice")
        .unwrap();
    s.remove_managed_group("db.read.empty", "alice").unwrap();
    assert!(s.managed_group("db.read.empty").unwrap().is_none());
}

#[test]
fn legacy_managed_group_display_name_falls_back_to_code() {
    let s = store();
    // display_name 列为默认空串的行（只有 name=code）。
    {
        let conn = s.db().lock().unwrap();
        conn.execute(
                "INSERT INTO groups (name, priority, min_budget, roles_json) VALUES ('legacy.group', 3, 800, '[]')",
                [],
            )
            .unwrap();
    }
    let g = s.managed_group("legacy.group").unwrap().expect("旧行可读");
    assert_eq!(g.code, "legacy.group");
    assert_eq!(g.name, "legacy.group", "空展示名回退 code");
    // 编辑只写展示列，主键 code 不变。
    let updated = s
        .update_managed_group(
            &ManagedGroup {
                code: "legacy.group".into(),
                name: "旧配置组".into(),
                priority: 4,
                min_budget: 900,
                roles: vec![],
                updated_at: String::new(),
            },
            "alice",
        )
        .unwrap();
    assert_eq!(updated.code, "legacy.group");
    assert_eq!(updated.name, "旧配置组");
    assert!(s.managed_group("legacy.group").unwrap().is_some());
}

#[test]
fn admin_audit_rejects_anonymous_actor() {
    let audit = AuditLog::open(":memory:").unwrap();
    let s = CapabilityStore::open(":memory:", Some(std::sync::Arc::new(audit))).unwrap();
    let e = s
        .write_admin_audit(AdminChange::new("capability", "cap1", "publish"))
        .unwrap_err();
    assert_eq!(e.code, "MCP_AUDIT_ERROR");
    assert!(
        s.audit.as_ref().unwrap().query(None).unwrap().is_empty(),
        "匿名 admin_change 不得落库"
    );
}

#[test]
fn definition_changes_flatten_nested_objects() {
    let before = json!({
        "template": {
            "description": "hash probe",
            "constraints": {"auto_limit": 1000},
            "params": [{"name": "x"}]
        },
        "removed": 1
    });
    let after = json!({
        "template": {
            "description": "hash probe v2",
            "constraints": {"auto_limit": 2000},
            "params": [{"name": "x"}]
        },
        "added": true
    });
    let changes = definition_changes(&before, &after);
    let row = |field: &str| {
        changes
            .iter()
            .find(|c| c["field"] == json!(field))
            .unwrap_or_else(|| panic!("缺字段 {field}: {changes:?}"))
    };
    assert_eq!(row("template.description")["before"], json!("hash probe"));
    assert_eq!(row("template.description")["after"], json!("hash probe v2"));
    assert_eq!(
        row("template.constraints.auto_limit")["before"],
        json!(1000)
    );
    assert_eq!(row("template.constraints.auto_limit")["after"], json!(2000));
    assert_eq!(row("removed")["after"], Value::Null);
    assert_eq!(row("added")["before"], Value::Null);
    assert!(
        !changes.iter().any(|c| c["field"] == json!("template")),
        "对象必须展开，不得整份输出: {changes:?}"
    );
}

#[test]
fn definition_changes_depth_cap_collapses_subtree() {
    fn nested(leaf: i64) -> Value {
        json!({"a":{"b":{"c":{"d":{"e":{"f":{"g": leaf}}}}}}})
    }
    let changes = definition_changes(&nested(1), &nested(2));
    assert_eq!(changes.len(), 1, "超过深度上限的子树整体输出: {changes:?}");
    assert_eq!(changes[0]["field"], json!("a.b.c.d.e.f"));
    assert_eq!(changes[0]["before"]["g"], json!(1));
    assert_eq!(changes[0]["after"]["g"], json!(2));
}

#[test]
fn group_writes_emit_admin_change_audit() {
    let audit = AuditLog::open(":memory:").unwrap();
    let s = CapabilityStore::open(":memory:", Some(std::sync::Arc::new(audit))).unwrap();
    s.insert_managed_group(&managed("db.read.audited", 5, 800, &["analyst"]), "alice")
        .unwrap();
    s.update_managed_group(&managed("db.read.audited", 6, 800, &["analyst"]), "bob")
        .unwrap();
    s.remove_managed_group("db.read.audited", "alice").unwrap();
    let rows = s.audit.as_ref().unwrap().query(None).unwrap();
    let changes: Vec<_> = rows
        .iter()
        .filter(|e| e.event_type == "admin_change")
        .collect();
    assert_eq!(changes.len(), 3, "create/update/delete 各一条");
    assert_eq!(changes[0].payload["object_type"], json!("group"));
    // 审计 object_id 仍是 code；data 里带 code/name/roles。
    assert_eq!(changes[0].payload["object_id"], json!("db.read.audited"));
    assert_eq!(changes[0].payload["action"], json!("group_create"));
    assert_eq!(changes[0].actor["id"], json!("alice"));
    assert_eq!(
        changes[0].payload["data"]["group"]["code"],
        json!("db.read.audited")
    );
    assert_eq!(
        changes[0].payload["data"]["group"]["name"],
        json!("db.read.audited 展示名"),
        "组审计 data 必须带展示名"
    );
    assert_eq!(
        changes[0].payload["data"]["group"]["roles"],
        json!(["analyst"])
    );
    let field = |row: &Value, name: &str| {
        row["changes"]
            .as_array()
            .and_then(|c| c.iter().find(|x| x["field"] == json!(name)))
            .cloned()
    };
    let created_name = field(&changes[0].payload, "name").expect("create 审计含 name");
    assert_eq!(created_name["before"], Value::Null);
    assert_eq!(created_name["after"], json!("db.read.audited 展示名"));
    let created_priority = field(&changes[0].payload, "priority").expect("create 审计含 priority");
    assert_eq!(created_priority["before"], Value::Null);
    assert_eq!(created_priority["after"], json!(5));
    assert_eq!(changes[1].payload["action"], json!("group_update"));
    assert_eq!(changes[1].payload["object_id"], json!("db.read.audited"));
    let updated_priority = field(&changes[1].payload, "priority").expect("update 审计含 priority");
    assert_eq!(updated_priority["before"], json!(5));
    assert_eq!(updated_priority["after"], json!(6));
    assert_eq!(changes[2].payload["action"], json!("group_delete"));
    let deleted_priority = field(&changes[2].payload, "priority").expect("delete 审计含 priority");
    assert_eq!(deleted_priority["after"], Value::Null);
    for change in changes {
        assert_eq!(change.payload["outcome"], json!("ok"));
        assert!(
            change.payload["changes"]
                .as_array()
                .is_some_and(|c| !c.is_empty())
        );
        assert_eq!(change.payload["approval"], Value::Null);
    }
}

#[test]
fn draft_revision_bumps_version_and_keeps_status() {
    let s = store();
    s.register(
        "cap1",
        "tool",
        "db.read.core",
        "read",
        json!({"description":"v1"}),
    )
    .unwrap();
    let r = s
        .revise_draft_definition(
            "cap1",
            json!({"description":"v2"}),
            "draft-edit:alice",
            "alice",
        )
        .unwrap();
    assert_eq!(r.version, 2);
    assert_eq!(r.status, "draft");
    assert_eq!(
        s.get_version("cap1", 1).unwrap().unwrap().definition["description"],
        json!("v1"),
        "旧版本必须原样保留（append-only）"
    );
    // 草案编辑不要求 approval_id；空票号原样留在版本行，不伪造审批。
    let r = s
        .revise_draft_definition("cap1", json!({"description":"v3"}), "", "alice")
        .unwrap();
    assert_eq!(r.version, 3);
    let v3 = s.versions("cap1").unwrap().pop().unwrap();
    assert_eq!(v3.approval_id, "");
    assert_eq!(v3.status, "draft");
    assert!(v3.approved_by.is_empty());
    assert!(!v3.created_at.is_empty());
    // 非 draft 状态拒绝。
    s.submit_for_review("cap1", "alice").unwrap();
    let e = s
        .revise_draft_definition("cap1", json!({"description":"v4"}), "ap", "alice")
        .unwrap_err();
    assert_eq!(e.code, "MCP_STATE_TRANSITION_INVALID");
}

#[test]
fn draft_revision_rejects_builtin() {
    let s = store();
    for id in BUILTIN_CAPABILITY_IDS {
        let e = s
            .revise_draft_definition(id, json!({}), "ap", "alice")
            .unwrap_err();
        assert_eq!(e.code, "MCP_BUILTIN_IMMUTABLE");
    }
}
