use super::*;

/// `GET /admin/me`：当前 token 派生出的管理主体（前端据此显示操作者，不自述身份）。
pub(super) async fn me(Extension(auth): Extension<AuthenticatedAdmin>) -> Json<Value> {
    Json(json!({"id": auth.id, "role": "admin", "type": "admin"}))
}

pub(super) async fn health(State(state): State<AdminState>) -> Json<Value> {
    let settings = &state.service.settings;
    let datasources: Vec<Value> = settings
        .datasources
        .iter()
        .map(|d| json!({"name": d.name, "engine": d.engine, "readonly": d.readonly}))
        .collect();
    // 链被篡改时 status 仍 200（服务在跑），但 audit_chain 显式报警，供看板直接着色。
    let (chain, tampered_at) = match &state.service.audit {
        Some(log) => match log.verify_chain() {
            Ok(ChainVerdict::Valid) => ("valid", Value::Null),
            Ok(ChainVerdict::Tampered { event_id }) => ("tampered", json!(event_id)),
            Ok(ChainVerdict::Broken { reason }) => ("error", json!(reason)),
            Err(e) => ("error", json!(e.message)),
        },
        None => ("error", json!("audit log not configured")),
    };
    let mut body = json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "datasources": datasources,
        "audit_chain": chain
    });
    if !tampered_at.is_null() {
        body["audit_chain_detail"] = tampered_at;
    }
    Json(body)
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
                limit = value.parse::<i64>().map_err(|_| {
                    ApiError(McpError::new(
                        "MCP_BAD_REQUEST",
                        json!({"field": "limit", "value": value}),
                    ))
                })?;
            }
            "offset" => {
                offset = value.parse::<i64>().map_err(|_| {
                    ApiError(McpError::new(
                        "MCP_BAD_REQUEST",
                        json!({"field": "offset", "value": value}),
                    ))
                })?;
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
            other if FILTER_KEYS.contains(&other) => {
                filters.insert(other.to_string(), json!(value));
            }
            _ => {
                let e = McpError::new(
                    "MCP_FILTER_UNSUPPORTED",
                    json!({"filter": key, "supported": FILTER_KEYS}),
                );
                return Err(reject(
                    &state,
                    &actor,
                    e,
                    json!({"surface": "admin", "filter": key}),
                ));
            }
        }
    }
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
