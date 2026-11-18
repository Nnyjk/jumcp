use super::*;

/// 统计窗口默认/上限（天）：越界夹紧而不是报错。
pub(super) const STATS_DEFAULT_WINDOW_DAYS: i64 = 7;
pub(super) const STATS_MAX_WINDOW_DAYS: i64 = 30;
/// 统计列表默认条数（上限与审计查询共用 `MAX_LIMIT`）。
pub(super) const STATS_DEFAULT_LIMIT: i64 = 200;
/// 单次审计扫描上限：任一事件类型命中该上限即把 `truncated` 置 true（窗口过大时如实标记，不静默）。
pub(super) const STATS_SCAN_LIMIT: i64 = 50_000;
/// `recent_tokens` 保留最近多少个非空 token 样本。
pub(super) const RECENT_TOKENS: usize = 20;
/// 单 id 明细的默认/上限窗口（天）：审计表只追加，明细必须带时间边界。
pub(super) const CALLS_DEFAULT_WINDOW_DAYS: i64 = 30;
pub(super) const CALLS_MAX_WINDOW_DAYS: i64 = 90;

/// 一条审计事件归一化成"一次能力调用"。
///
/// `tool_call` 的能力 id 取 `payload.tool`（任意非空工具名：常驻工具、维护工具以及未来扩展的
/// 内置工具都自动纳入台账），`template_run` 取 `payload.template_id`；其它事件类型与缺 id 的
/// 行直接丢弃。`capability_search` / `capability_describe` 是独立事件类型，不算能力调用。
/// 能力统计与单 id 明细共用这一处解析，避免两个接口对同一批事件给出不同结论。
#[derive(Debug, Clone)]
pub(super) struct CallObservation {
    id: String,
    event_id: String,
    ts: String,
    session_id: String,
    event_type: String,
    object_type: String,
    actor: Value,
    outcome: String,
    tokens: Option<i64>,
    cache: Option<String>,
    elapsed_ms: Option<i64>,
    version: Option<i64>,
    error_code: Option<String>,
    error_message: Option<String>,
    arguments: Value,
    executed_script: Option<String>,
    bound_params: Value,
}

/// 调用对象 id：只读结构化载荷的 `object_id`（没有该字段的事件不参与台账/明细）。
pub(super) fn observation_id(env: &Envelope) -> Option<&str> {
    match env.event_type.as_str() {
        "template_run" | "tool_call" => env
            .payload
            .get("object_id")?
            .as_str()
            .filter(|id| !id.is_empty()),
        _ => None,
    }
}

pub(super) fn observation(env: &Envelope) -> Option<CallObservation> {
    if !matches!(env.event_type.as_str(), "tool_call" | "template_run") {
        return None;
    }
    let id = observation_id(env)?.to_string();
    let data = env
        .payload
        .get("data")
        .cloned()
        .unwrap_or_else(|| json!({}));
    Some(CallObservation {
        id,
        event_id: env.event_id.clone(),
        ts: env.ts.clone(),
        session_id: env.session_id.clone(),
        event_type: env.event_type.clone(),
        object_type: env
            .payload
            .get("object_type")
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_string(),
        actor: env.actor.clone(),
        outcome: env
            .payload
            .get("outcome")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        tokens: data.get("tokens").and_then(|v| v.as_i64()),
        cache: data.get("cache").and_then(|v| v.as_str()).map(String::from),
        elapsed_ms: data.get("elapsed_ms").and_then(|v| v.as_i64()),
        version: data.get("version").and_then(|v| v.as_i64()),
        error_code: env
            .payload
            .get("error_code")
            .and_then(|v| v.as_str())
            .map(String::from),
        error_message: env
            .payload
            .get("error_message")
            .and_then(|v| v.as_str())
            .map(String::from),
        arguments: data.get("arguments").cloned().unwrap_or(Value::Null),
        executed_script: data
            .get("executed_script")
            .and_then(|v| v.as_str())
            .map(String::from),
        bound_params: data.get("bound_params").cloned().unwrap_or(Value::Null),
    })
}

/// 窗口内按事件类型扫描（单次上限 `STATS_SCAN_LIMIT`）；返回 (观测, 是否命中扫描上限)。
///
/// `surface_not: "admin"` 是统计口径的核心：管理台的每次页面操作都会产生审计事件
/// （`/admin/tools/stats` 自己就写 `policy_decision`/`admin_change`），把它们算进
/// "工具调用排行"等于让看板给自己刷量。审计**查询**接口不受影响，平台面事件照旧可查。
pub(super) fn scan_in_window(
    log: &AuditLog,
    event_type: &str,
    ts_after: &str,
) -> Result<(Vec<CallObservation>, bool), McpError> {
    let filter = json!({"event_type": event_type, "ts_after": ts_after, "surface_not": "admin"});
    let (rows, _total) = log.query_page(Some(&filter), STATS_SCAN_LIMIT, 0, Order::Asc)?;
    let truncated = rows.len() as i64 >= STATS_SCAN_LIMIT;
    Ok((rows.iter().filter_map(observation).collect(), truncated))
}

/// 单个 id 的明细扫描：按事件类型翻页，只保留该 id 的事件。
///
/// 为什么带时间窗：审计表只追加，不带边界的明细查询会随运行时间线性变慢。
/// 为什么不用一次全表 `query`：明细页不该把整张审计表读进内存；翻页时逐页过滤，
/// 内存只与"这个 id 在窗口内的调用次数"成正比，`total` 仍是精确值。
/// 返回 `Envelope` 而非投影：明细要原样回放入参/脚本，投影会丢掉 payload 其余内容。
pub(super) fn scan_by_id(
    log: &AuditLog,
    event_type: &str,
    id: &str,
    ts_after: &str,
) -> Result<Vec<Envelope>, McpError> {
    const PAGE: i64 = 5_000;
    // 与 `scan_in_window` 同一口径：平台面事件不计入工具调用明细/排名。
    let filter = json!({"event_type": event_type, "ts_after": ts_after, "surface_not": "admin"});
    let mut out: Vec<Envelope> = Vec::new();
    let mut offset = 0i64;
    loop {
        let (rows, _total) = log.query_page(Some(&filter), PAGE, offset, Order::Asc)?;
        let got = rows.len() as i64;
        out.extend(
            rows.into_iter()
                .filter(|env| observation_id(env) == Some(id)),
        );
        if got < PAGE {
            return Ok(out);
        }
        offset += got;
    }
}

/// 统计对象的元信息：注册表全部行 + 全部 builtin（含 admin_*）+ 仅出现在审计里的 id。
pub(super) struct ToolMeta {
    kind: String,
    group: String,
    /// 分类标签（`basic|analysis|admin|custom`）：统计台账与发现面同一映射。
    category: String,
    lifecycle_status: String,
    risk: Option<String>,
    discoverable: Option<bool>,
    callable: Option<bool>,
    version: Option<i64>,
    last_changed_at: Option<String>,
    source: String,
}

pub(super) fn tool_universe(
    store: &CapabilityStore,
) -> Result<HashMap<String, ToolMeta>, McpError> {
    let mut out: HashMap<String, ToolMeta> = HashMap::new();
    // builtin 定义只在代码里：分组沿用发现面的同一份常量（常驻="basic"，维护="admin"）。
    for id in BUILTIN_CAPABILITY_IDS {
        let group = if ADMIN_CAPABILITY_IDS.contains(id) {
            crate::discovery::ADMIN_GROUP
        } else {
            crate::discovery::BASIC_GROUP
        };
        out.insert(
            (*id).to_string(),
            ToolMeta {
                kind: "tool".into(),
                group: group.into(),
                lifecycle_status: "published".into(),
                category: crate::mcp::builtin_category(id).to_string(),
                risk: Some("read".into()),
                discoverable: Some(true),
                callable: Some(true),
                version: None,
                last_changed_at: None,
                source: "builtin".into(),
            },
        );
    }
    // 注册表行不能影子覆盖 builtin（store 本身也拒写 builtin id）。
    for rec in store.all()? {
        out.entry(rec.capability_id).or_insert(ToolMeta {
            kind: rec.kind,
            group: rec.group,
            category: crate::catalog::CATEGORY_ANALYSIS.to_string(),
            lifecycle_status: rec.status,
            risk: Some(rec.risk),
            discoverable: Some(rec.discoverable),
            callable: Some(rec.callable),
            version: Some(rec.version),
            last_changed_at: (!rec.updated_at.is_empty()).then_some(rec.updated_at),
            source: "managed".into(),
        });
    }
    Ok(out)
}

#[derive(Default)]
pub(super) struct ToolAggregate {
    calls: i64,
    success: i64,
    failure: i64,
    cache_hits: i64,
    retries: i64,
    /// 非空 tokens 样本，按时间升序。
    tokens: Vec<i64>,
    last_call_at: Option<String>,
    last_outcome: Option<String>,
}

/// 单个能力的聚合口径（契约，测试锁定）：
/// - success = outcome ∈ {ok, summary}；failure = outcome ∈ {error, unknown, budget_rejected}；
///   其余 outcome 只计入 calls，不臆造成成功或失败。
/// - retries：按 `(session_id, id)` 分组（组内按时间升序），某次 outcome ∈ {error, unknown}
///   **且该会话内存在紧邻的下一次调用**时计 1（失败后确实在同一轮里又试了一次）；
///   其它会话的调用不得吞掉这个判定（`list` 是全局时间序，必须先分组）。
/// - tokens：按时间升序收集；`recent_tokens` 取最后 `RECENT_TOKENS` 个（保持时间升序）。
pub(super) fn aggregate(list: &[CallObservation]) -> ToolAggregate {
    let mut agg = ToolAggregate {
        calls: list.len() as i64,
        ..Default::default()
    };
    for o in list {
        match o.outcome.as_str() {
            "ok" | "summary" => agg.success += 1,
            "error" | "unknown" | "budget_rejected" => agg.failure += 1,
            _ => {}
        }
    }
    agg.cache_hits = list
        .iter()
        .filter(|o| o.cache.as_deref() == Some("hit"))
        .count() as i64;
    agg.tokens = list.iter().filter_map(|o| o.tokens).collect();
    agg.last_call_at = list.last().map(|o| o.ts.clone());
    agg.last_outcome = list.last().map(|o| o.outcome.clone());
    // 重试必须按 `(session_id, id)` 分组后判：`list` 是全局按 ts 排序的，其它会话的调用
    // 会插在两次尝试之间；只有组内（已按时间升序）紧邻的下一次才算"同一轮内的再次尝试"。
    let mut by_session: HashMap<&str, Vec<&CallObservation>> = HashMap::new();
    for o in list {
        by_session.entry(o.session_id.as_str()).or_default().push(o);
    }
    agg.retries = by_session
        .values()
        .map(|seq| {
            seq.windows(2)
                .filter(|w| matches!(w[0].outcome.as_str(), "error" | "unknown"))
                .count() as i64
        })
        .sum();
    agg
}

pub(super) fn round_to(value: f64, digits: i32) -> f64 {
    let factor = 10f64.powi(digits);
    (value * factor).round() / factor
}

/// 最近秩法：rank = ceil(0.95 * n)，取第 rank 小（1-based）。样本非空。
///
/// `samples` 是按时间收集的，取分位数前必须复制并升序排序；直接按原序取位会得到
/// “第几次调用”的 token 数而不是分位数。
pub(super) fn percentile95(samples: &[i64]) -> i64 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    let rank = (95 * n).div_ceil(100);
    sorted[rank.saturating_sub(1).min(n - 1)]
}

pub(super) fn health_of(calls: i64, success: i64) -> &'static str {
    if calls == 0 {
        return "unknown";
    }
    let rate = success as f64 / calls as f64;
    if rate >= 0.95 {
        "ok"
    } else if rate >= 0.80 {
        "degraded"
    } else {
        "failing"
    }
}

pub(super) fn null_or<T: Into<Value>>(value: Option<T>) -> Value {
    value.map_or(Value::Null, |v| v.into())
}

pub(super) fn tool_stats_item(id: &str, meta: &ToolMeta, list: &[CallObservation]) -> Value {
    let agg = aggregate(list);
    let calls = agg.calls;
    let success_rate = if calls == 0 {
        Value::Null
    } else {
        json!(round_to(agg.success as f64 / calls as f64, 4))
    };
    // 命中率只对模板有意义：非 template 或窗口内无调用时给 null（不是 0）。
    let hit_rate = if meta.kind == "template" && calls > 0 {
        json!(round_to(agg.cache_hits as f64 / calls as f64, 4))
    } else {
        Value::Null
    };
    let recent_tokens: Vec<i64> = agg
        .tokens
        .iter()
        .rev()
        .take(RECENT_TOKENS)
        .rev()
        .copied()
        .collect();
    json!({
        "id": id,
        "kind": meta.kind,
        "group": meta.group,
        "category": meta.category,
        "lifecycle_status": meta.lifecycle_status,
        "risk": null_or(meta.risk.clone()),
        "discoverable": null_or(meta.discoverable),
        "callable": null_or(meta.callable),
        "version": null_or(meta.version),
        "last_changed_at": null_or(meta.last_changed_at.clone()),
        "source": meta.source,
        "health": health_of(calls, agg.success),
        "calls": calls,
        "success": agg.success,
        "failure": agg.failure,
        "success_rate": success_rate,
        "cache_hits": agg.cache_hits,
        "hit_rate": hit_rate,
        "retries": agg.retries,
        "avg_response_tokens": if agg.tokens.is_empty() {
            Value::Null
        } else {
            json!(round_to(agg.tokens.iter().sum::<i64>() as f64 / agg.tokens.len() as f64, 1))
        },
        "p95_response_tokens": if agg.tokens.is_empty() {
            Value::Null
        } else {
            json!(percentile95(&agg.tokens))
        },
        "max_response_tokens": null_or(agg.tokens.iter().max().copied()),
        "last_call_at": null_or(agg.last_call_at),
        "last_outcome": null_or(agg.last_outcome),
        "recent_tokens": recent_tokens,
    })
}

pub(super) async fn tool_stats(
    State(state): State<AdminState>,
    Query(raw): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let actor = admin_actor("");
    let mut window_days = STATS_DEFAULT_WINDOW_DAYS;
    let mut limit = STATS_DEFAULT_LIMIT;
    for (key, value) in raw {
        match key.as_str() {
            "window_days" => {
                window_days =
                    parse_page_param(&state, &actor, "admin/tools/stats", "window_days", &value)?;
            }
            "limit" => {
                limit = parse_page_param(&state, &actor, "admin/tools/stats", "limit", &value)?;
            }
            _ => {
                let e = McpError::new(
                    "MCP_FILTER_UNSUPPORTED",
                    json!({"filter": key, "supported": ["window_days", "limit"]}),
                );
                return Err(reject(
                    &state,
                    &actor,
                    e,
                    json!({"surface": "admin/tools/stats", "filter": key}),
                ));
            }
        }
    }
    let window_days = window_days.clamp(1, STATS_MAX_WINDOW_DAYS);
    let limit = limit.clamp(1, MAX_LIMIT);
    let to = Utc::now();
    let from = to - chrono::Duration::days(window_days);
    let from_iso = from.to_rfc3339();
    let to_iso = to.to_rfc3339();

    let Some(log) = state.service.audit.clone() else {
        return Err(ApiError(McpError::new(
            "MCP_AUDIT_ERROR",
            json!({"hint": "审计未启用"}),
        )));
    };
    let mut truncated = false;
    let mut observations: Vec<CallObservation> = Vec::new();
    for event_type in ["tool_call", "template_run"] {
        let (mut rows, cut) = scan_in_window(&log, event_type, &from_iso)
            .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/tools/stats"})))?;
        truncated |= cut;
        observations.append(&mut rows);
    }
    // 时间升序；等 ts 时保持扫描顺序（rowid 顺序 = 审计写入顺序）。
    observations.sort_by(|a, b| a.ts.cmp(&b.ts));

    let mut universe = tool_universe(&state.store)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/tools/stats"})))?;
    let mut by_id: HashMap<String, Vec<CallObservation>> = HashMap::new();
    for o in observations {
        by_id.entry(o.id.clone()).or_default().push(o);
    }
    // 不在注册表也不在 builtin 里的 id 也要出现一行（审计里确实被调用过）。
    for id in by_id.keys() {
        universe.entry(id.clone()).or_insert(ToolMeta {
            kind: "unknown".into(),
            group: String::new(),
            category: crate::catalog::CATEGORY_CUSTOM.to_string(),
            lifecycle_status: "unknown".into(),
            risk: None,
            discoverable: None,
            callable: None,
            version: None,
            last_changed_at: None,
            source: "managed".into(),
        });
    }

    let mut rows: Vec<(i64, String, Value)> = universe
        .iter()
        .map(|(id, meta)| {
            let list = by_id.get(id).map_or(&[][..], |v| v.as_slice());
            let item = tool_stats_item(id, meta, list);
            let calls = item["calls"].as_i64().unwrap_or_default();
            (calls, id.clone(), item)
        })
        .collect();
    // 契约排序：calls 降序、其次 id 升序；超过 limit 即截断并置 truncated。
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    if rows.len() > limit as usize {
        truncated = true;
    }
    let tools: Vec<Value> = rows
        .into_iter()
        .take(limit as usize)
        .map(|(_, _, item)| item)
        .collect();

    Ok(Json(json!({
        "window": {"days": window_days, "from": from_iso, "to": to_iso},
        "truncated": truncated,
        "tools": tools
    })))
}

/// `GET /admin/tools/{id}/calls`：单个能力的调用明细（时间倒序，缺字段一律 null）。
///
/// `window_days` 默认 30、夹紧 1..=90：响应结构不变，但 `total` 的语义是
/// "窗口内该 id 的调用数"（审计表只追加，明细查询必须带时间边界才不会越跑越慢）。
/// 调用明细的主体投影：`id`/`role` 恒有；审计 actor 里带 `source`/`token_id` 时原样带出。
///
/// 没有 token 归属的事件不带这两个键，投影时直接省略（消费方按可选字段处理）。
pub(super) fn call_actor_json(actor: &Value) -> Value {
    let mut out = json!({
        "id": actor.get("id").cloned().unwrap_or(Value::Null),
        "role": actor.get("role").cloned().unwrap_or(Value::Null),
    });
    if let Some(obj) = out.as_object_mut() {
        for key in ["source", "token_id"] {
            if let Some(value) = actor.get(key).filter(|v| !v.is_null()) {
                obj.insert(key.to_string(), value.clone());
            }
        }
    }
    out
}

pub(super) async fn tool_calls(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Query(raw): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let actor = admin_actor("");
    let mut limit = DEFAULT_LIMIT;
    let mut offset = 0i64;
    let mut window_days = CALLS_DEFAULT_WINDOW_DAYS;
    for (key, value) in raw {
        match key.as_str() {
            "limit" => {
                limit = parse_page_param(&state, &actor, "admin/tools/calls", "limit", &value)?
            }
            "offset" => {
                offset = parse_page_param(&state, &actor, "admin/tools/calls", "offset", &value)?
            }
            "window_days" => {
                window_days =
                    parse_page_param(&state, &actor, "admin/tools/calls", "window_days", &value)?
            }
            _ => {
                let e = McpError::new(
                    "MCP_FILTER_UNSUPPORTED",
                    json!({"filter": key, "supported": ["limit", "offset", "window_days"]}),
                );
                return Err(reject(
                    &state,
                    &actor,
                    e,
                    json!({"surface": "admin/tools/calls", "filter": key}),
                ));
            }
        }
    }
    let limit = limit.clamp(1, MAX_LIMIT);
    let offset = offset.max(0);
    let window_days = window_days.clamp(1, CALLS_MAX_WINDOW_DAYS);
    let ts_after = (Utc::now() - chrono::Duration::days(window_days)).to_rfc3339();
    let Some(log) = state.service.audit.clone() else {
        return Err(ApiError(McpError::new(
            "MCP_AUDIT_ERROR",
            json!({"hint": "审计未启用"}),
        )));
    };
    let mut observations = scan_by_id(&log, "tool_call", &id, &ts_after)
        .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/tools/calls"})))?;
    observations.extend(
        scan_by_id(&log, "template_run", &id, &ts_after)
            .map_err(|e| reject(&state, &actor, e, json!({"surface": "admin/tools/calls"})))?,
    );
    // 时间倒序；等 ts 时后写入的排前面（先升序稳定排序，再整体反转）。
    observations.sort_by(|a, b| a.ts.cmp(&b.ts));
    observations.reverse();

    let total = observations.len();
    let calls: Vec<Value> = observations
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .map(|env| {
            // scan_by_id 只返回 tool_call/template_run 且 id 匹配，投影必然成功。
            let o = observation(&env).expect("scan_by_id 返回的事件必可投影");
            json!({
                "event_id": o.event_id,
                "ts": o.ts,
                "event_type": o.event_type,
                "object_type": o.object_type,
                "object_id": o.id,
                "actor": call_actor_json(&o.actor),
                "outcome": o.outcome,
                "tokens": null_or(o.tokens),
                "cache": null_or(o.cache),
                "elapsed_ms": null_or(o.elapsed_ms),
                "version": null_or(o.version),
                "error_code": null_or(o.error_code),
                "error_message": null_or(o.error_message),
                "arguments": o.arguments,
                "executed_script": null_or(o.executed_script),
                "bound_params": o.bound_params
            })
        })
        .collect();
    Ok(Json(json!({
        "id": id,
        "total": total,
        "limit": limit,
        "offset": offset,
        "calls": calls
    })))
}
