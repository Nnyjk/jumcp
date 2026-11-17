use super::*;

pub(super) fn capability_item(rec: &CapabilityRecord) -> Value {
    json!({
        "id": rec.capability_id,
        "kind": rec.kind,
        "group": rec.group,
        "risk": rec.risk,
        "status": rec.status,
        "discoverable": rec.discoverable,
        "callable": rec.callable,
        "version": rec.version,
        "definition_hash": rec.definition_hash,
        "successor_id": rec.successor_id,
        "last_changed_at": non_empty(&rec.updated_at),
        "submitted_by": non_empty(&rec.submitted_by),
        "submitted_at": non_empty(&rec.submitted_at)
    })
}

/// `GET /admin/capabilities/{id}`：摘要字段 + `definition` 原文。
///
/// 只读：不写审计（读详情不构成治理变更）。内置 id 没有注册表行 → 404（与不存在同构）。
pub(super) async fn capability_detail(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let record = state.store.get(&id).map_err(ApiError)?.ok_or_else(|| {
        ApiError(McpError::new(
            "MCP_CAPABILITY_NOT_FOUND",
            json!({"capability_id": id}),
        ))
    })?;
    let mut item = capability_item(&record);
    item["definition"] = record.definition;
    item["definition_updated_at"] = state
        .store
        .latest_version_created_at(&id)
        .map_err(ApiError)?
        .map_or(Value::Null, |s| non_empty(&s));
    Ok(Json(item))
}

/// `GET /admin/capabilities/{id}/versions`：版本历史（升序）。没有版本行时给空数组。
pub(super) async fn capability_versions(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let rows = state.store.versions(&id).map_err(ApiError)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|v| {
            json!({
                "version": v.version,
                "definition": v.definition,
                "definition_hash": v.definition_hash,
                "status": non_empty(&v.status),
                "created_at": non_empty(&v.created_at),
                "submitted_by": non_empty(&v.submitted_by),
                "submitted_at": non_empty(&v.submitted_at),
                "approved_by": non_empty(&v.approved_by),
                "approved_at": non_empty(&v.approved_at),
                "approval_id": non_empty(&v.approval_id)
            })
        })
        .collect();
    Ok(Json(json!({"items": items, "total": items.len()})))
}

/// 列表页只需要摘要：description 参与 `q` 匹配，但完整 definition 不回传（避免把 schema 塞进管理面）。
pub(super) fn description_of(rec: &CapabilityRecord) -> String {
    rec.definition
        .get("description")
        .and_then(|d| d.as_str())
        .or_else(|| {
            rec.definition
                .get("template")
                .and_then(|t| t.get("description"))
                .and_then(|d| d.as_str())
        })
        .unwrap_or_default()
        .to_string()
}

pub(super) async fn capabilities(
    State(state): State<AdminState>,
    Query(raw): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let actor = admin_actor("");
    let mut statuses: Vec<String> = Vec::new();
    let mut group: Option<String> = None;
    let mut query: Option<String> = None;
    let mut limit = DEFAULT_LIMIT;
    let mut offset = 0i64;
    for (key, value) in raw {
        match key.as_str() {
            "status" => {
                statuses = value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect();
                if let Some(bad) = statuses.iter().find(|s| !STATUSES.contains(&s.as_str())) {
                    let e = McpError::new(
                        "MCP_BAD_REQUEST",
                        json!({"field": "status", "value": bad, "supported": STATUSES}),
                    );
                    return Err(reject(
                        &state,
                        &actor,
                        e,
                        json!({"surface": "admin/capabilities", "field": "status"}),
                    ));
                }
            }
            "group" => group = Some(value),
            "q" => query = Some(value.to_lowercase()),
            "limit" => {
                limit = parse_page_param(&state, &actor, "admin/capabilities", "limit", &value)?
            }
            "offset" => {
                offset = parse_page_param(&state, &actor, "admin/capabilities", "offset", &value)?
            }
            _ => {
                let e = McpError::new(
                    "MCP_FILTER_UNSUPPORTED",
                    json!({"filter": key, "supported": ["status","group","q","limit","offset"]}),
                );
                return Err(reject(
                    &state,
                    &actor,
                    e,
                    json!({"surface": "admin/capabilities", "filter": key}),
                ));
            }
        }
    }
    let limit = limit.clamp(1, MAX_LIMIT);
    let offset = offset.max(0);
    let all = state
        .store
        .all()
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/capabilities"})))?;
    let filtered: Vec<&CapabilityRecord> = all
        .iter()
        .filter(|rec| statuses.is_empty() || statuses.contains(&rec.status))
        .filter(|rec| group.as_ref().is_none_or(|g| &rec.group == g))
        .filter(|rec| {
            query.as_ref().is_none_or(|needle| {
                rec.capability_id.to_lowercase().contains(needle)
                    || description_of(rec).to_lowercase().contains(needle)
            })
        })
        .collect();
    let total = filtered.len() as i64;
    let items: Vec<Value> = filtered
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .map(capability_item)
        .collect();
    Ok(Json(json!({
        "items": items,
        "total": total,
        "limit": limit,
        "offset": offset
    })))
}
