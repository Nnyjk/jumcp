use super::*;

/// `GET /admin/me`：会话所属的管理主体（前端据此显示操作者，不自述身份）。
///
/// 形状必须与 `/admin/session` 的 `actor` 逐字段一致（同一身份两种入口）。
pub(super) async fn me(Extension(auth): Extension<AuthenticatedAdmin>) -> Json<Value> {
    Json(json!({"id": auth.id, "user_id": auth.user_id, "role": "admin", "type": "admin"}))
}

pub(super) async fn health(State(state): State<AdminState>) -> Json<Value> {
    Json(crate::service::health_report(&state.service))
}

pub(super) async fn audit_events(
    State(state): State<AdminState>,
    Query(raw): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let actor = admin_actor("");
    let mut filters = Map::new();
    let mut limit = DEFAULT_LIMIT;
    let mut offset = 0i64;
    let mut order = Order::Desc;
    for (key, value) in raw {
        match key.as_str() {
            "limit" => {
                limit = parse_page_param(&state, &actor, "admin/audit-events", "limit", &value)?;
            }
            "offset" => {
                offset = parse_page_param(&state, &actor, "admin/audit-events", "offset", &value)?;
            }
            "order" => {
                order = Order::parse(&value).map_err(|e| {
                    reject(
                        &state,
                        &actor,
                        e,
                        json!({"surface": "admin/audit-events", "field": "order"}),
                    )
                })?;
            }
            // 面筛选：`all` 等价缺省（不插过滤），因此历史行 `surface=''` 也可见。
            "surface" => match value.as_str() {
                "admin" | "mcp" => {
                    filters.insert("surface".to_string(), json!(value));
                }
                "all" => {}
                _ => {
                    let e = McpError::new(
                        "MCP_BAD_REQUEST",
                        json!({
                            "field": "surface",
                            "value": value,
                            "supported": SURFACES,
                        }),
                    );
                    return Err(reject(
                        &state,
                        &actor,
                        e,
                        json!({"surface": "admin/audit-events", "field": "surface"}),
                    ));
                }
            },
            other if FILTER_KEYS.contains(&other) => {
                filters.insert(other.to_string(), json!(value));
            }
            _ => {
                // `surface` 走上面的单独分支校验取值，但仍是过滤契约的一部分：报错时一并列出，
                // 否则前端照着 `supported` 生成筛选器会把面筛选漏掉。
                let mut supported = FILTER_KEYS.to_vec();
                supported.push("surface");
                let e = McpError::new(
                    "MCP_FILTER_UNSUPPORTED",
                    json!({"filter": key, "supported": supported}),
                );
                return Err(reject(
                    &state,
                    &actor,
                    e,
                    json!({"surface": "admin/audit-events", "filter": key}),
                ));
            }
        }
    }
    // 缺省不按面过滤：agent 发起的治理写（`admin_change`：注册模板 / 改可见性）与 MCP 工具
    // 调用同属审计事实，管理台"从审计提取高频查询 → 定制化"必须两者都看得见。
    // MCP 鉴权噪音用 `event_type` / `actor_id` 收窄，不由 surface 隐藏。
    // 超上限夹紧而不是报错：管理面分页是展示需求，不构成契约破坏。
    let limit = limit.clamp(1, MAX_LIMIT);
    let offset = offset.max(0);
    let Some(log) = state.service.audit.clone() else {
        return Err(ApiError(McpError::new(
            "MCP_AUDIT_ERROR",
            json!({"hint": "审计未启用"}),
        )));
    };
    let (events, total) = log
        .query_page(Some(&Value::Object(filters)), limit, offset, order)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/audit-events"})))?;
    Ok(Json(json!({
        "events": events,
        "total": total,
        "limit": limit,
        "offset": offset,
        "order": if order == Order::Asc { "asc" } else { "desc" }
    })))
}

/// 审核队列条目的唯一形状：HTTP 管理面与 MCP `admin_review_queue` 共用。
pub(crate) fn queue_item(rec: &CapabilityRecord) -> Value {
    json!({
        "id": rec.capability_id,
        "kind": rec.kind,
        "group": rec.group,
        "risk": rec.risk,
        "status": rec.status,
        "version": rec.version,
        "definition_hash": rec.definition_hash,
        "submitted_by": non_empty(&rec.submitted_by),
        "submitted_at": non_empty(&rec.submitted_at),
        "last_changed_at": non_empty(&rec.updated_at)
    })
}

pub(super) async fn review_queue(State(state): State<AdminState>) -> Result<Json<Value>, ApiError> {
    let actor = admin_actor("");
    let records = state
        .store
        .review_queue()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/review-queue"})))?;
    let items: Vec<Value> = records.iter().map(queue_item).collect();
    Ok(Json(json!({"items": items, "total": items.len()})))
}
