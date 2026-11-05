//! 预算模型：常驻 2000 / 单次 1500 / 会话累计 2500 / schema 12 / 峰值 7500，默认 fail。
//! 为什么累计按会话：schema/result 常驻 transcript，按轮计会先破（§5.1）。

use crate::canonical::canonical_json;
use crate::errors::McpError;
use serde_json::{Value, json};
use std::collections::HashMap;

pub const RESIDENT_META_TOOLS: i64 = 400;
pub const RESIDENT_DISPATCHERS: i64 = 600;
pub const RESIDENT_GROUP_INDEX: i64 = 300;
pub const RESIDENT_OTHER: i64 = 400;
pub const RESIDENT_TOTAL_LIMIT: i64 = 2000;
pub const MAX_SCHEMA_COUNT: i64 = 12;
pub const MAX_SCHEMA_TOKENS: i64 = 3000;
pub const MAX_RESULT_SINGLE: i64 = 1500;
pub const MAX_RESULT_CUMULATIVE: i64 = 2500;
pub const PEAK_LIMIT: i64 = RESIDENT_TOTAL_LIMIT + MAX_SCHEMA_TOKENS + MAX_RESULT_CUMULATIVE;
pub const MIN_VIABLE_BUDGET: i64 = 800;

/// token 估算唯一口径：canonical JSON UTF-8 字节数 / 4，至少 1。
pub fn estimate_tokens(payload: &Value) -> i64 {
    std::cmp::max(1, canonical_json(payload).len() as i64 / 4)
}

#[derive(Debug, Clone)]
pub struct BudgetCaps {
    pub resident_limit: i64,
    pub max_schema_count: i64,
    pub max_schema_tokens: i64,
    pub max_result_single: i64,
    pub max_result_cumulative: i64,
    pub peak_limit: i64,
}
impl Default for BudgetCaps {
    fn default() -> Self {
        Self {
            resident_limit: RESIDENT_TOTAL_LIMIT,
            max_schema_count: MAX_SCHEMA_COUNT,
            max_schema_tokens: MAX_SCHEMA_TOKENS,
            max_result_single: MAX_RESULT_SINGLE,
            max_result_cumulative: MAX_RESULT_CUMULATIVE,
            peak_limit: PEAK_LIMIT,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionLedger {
    pub schema_count: i64,
    pub schema_tokens: i64,
    pub result_tokens: i64,
}
impl SessionLedger {
    pub fn in_summary_mode(&self) -> bool {
        self.result_tokens >= MAX_RESULT_CUMULATIVE
    }
}

#[derive(Debug, Clone)]
pub struct BudgetCheckResult {
    pub allowed: bool,
    pub error_code: String,
    pub suggestions: Vec<String>,
    pub dropped_groups: Vec<String>,
    pub summary: bool,
    pub detail: Value,
}
impl BudgetCheckResult {
    fn ok(summary: bool) -> Self {
        Self {
            allowed: true,
            error_code: String::new(),
            suggestions: vec![],
            dropped_groups: vec![],
            summary,
            detail: json!({}),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowPolicy {
    Fail,
    Truncate,
}

pub struct BudgetLedger {
    caps: BudgetCaps,
    ledgers: HashMap<String, SessionLedger>,
}
impl BudgetLedger {
    pub fn new(caps: Option<BudgetCaps>) -> Self {
        Self {
            caps: caps.unwrap_or_default(),
            ledgers: HashMap::new(),
        }
    }
    fn ledger(&mut self, session_id: &str) -> &mut SessionLedger {
        self.ledgers.entry(session_id.to_string()).or_default()
    }
    pub fn reset(&mut self, session_id: &str) {
        self.ledgers.remove(session_id);
    }
    pub fn record_schema(&mut self, session_id: &str, token_cost: i64) {
        let l = self.ledger(session_id);
        l.schema_count += 1;
        l.schema_tokens += token_cost;
    }
    pub fn record_result(&mut self, session_id: &str, token_cost: i64) {
        self.ledger(session_id).result_tokens += token_cost;
    }
    pub fn peak(&self, session_id: &str, resident_tokens: i64) -> i64 {
        let l = self.ledgers.get(session_id);
        resident_tokens + l.map(|x| x.schema_tokens + x.result_tokens).unwrap_or(0)
    }
    pub fn check_schema(
        &mut self,
        session_id: &str,
        incoming_tokens: i64,
        policy: OverflowPolicy,
    ) -> BudgetCheckResult {
        let l = self.ledger(session_id).clone();
        let projected_count = l.schema_count + 1;
        let projected_tokens = l.schema_tokens + incoming_tokens;
        if projected_count > self.caps.max_schema_count
            || projected_tokens > self.caps.max_schema_tokens
        {
            let suggestions = vec![
                "改用模板调用（db_run_template / invoke_capability），不新增 schema 注入"
                    .to_string(),
                format!(
                    "当前 {} 个 schema/{} tokens 已达上限 {}/3000，可通过缩小 X-MCP-Tool-Groups 释放组索引预算",
                    l.schema_count, l.schema_tokens, MAX_SCHEMA_COUNT
                ),
            ];
            if policy == OverflowPolicy::Truncate {
                return BudgetCheckResult {
                    allowed: true,
                    error_code: String::new(),
                    suggestions: vec![
                        "truncate 已开启：返回 truncated 标记与 dropped_groups，不新增注入"
                            .to_string(),
                    ],
                    dropped_groups: vec![],
                    summary: l.in_summary_mode(),
                    detail: json!({"projected_count": projected_count, "projected_tokens": projected_tokens}),
                };
            }
            return BudgetCheckResult {
                allowed: false,
                error_code: "MCP_CONTEXT_BUDGET_EXHAUSTED".to_string(),
                suggestions,
                dropped_groups: vec![],
                summary: l.in_summary_mode(),
                detail: json!({"schema_count": l.schema_count, "schema_tokens": l.schema_tokens, "projected_count": projected_count, "projected_tokens": projected_tokens}),
            };
        }
        BudgetCheckResult::ok(l.in_summary_mode())
    }
    pub fn check_result(&mut self, session_id: &str, incoming_tokens: i64) -> BudgetCheckResult {
        let l = self.ledger(session_id).clone();
        let projected = l.result_tokens + incoming_tokens;
        let summary = l.in_summary_mode() || projected > self.caps.max_result_cumulative;
        if incoming_tokens > self.caps.max_result_single {
            return BudgetCheckResult {
                allowed: true,
                error_code: String::new(),
                suggestions: vec![],
                dropped_groups: vec![],
                summary,
                detail: json!({"truncated": true, "single": incoming_tokens, "cumulative": l.result_tokens}),
            };
        }
        BudgetCheckResult {
            allowed: true,
            error_code: String::new(),
            suggestions: vec![],
            dropped_groups: vec![],
            summary,
            detail: json!({"single": incoming_tokens, "cumulative": l.result_tokens, "projected": projected}),
        }
    }
    /// 超限默认抛 MCP_BUDGET_EXCEEDED；truncate 显式开启才放行。
    pub fn check_resident(
        &self,
        resident_tokens: i64,
        policy: OverflowPolicy,
    ) -> Result<BudgetCheckResult, McpError> {
        if resident_tokens <= self.caps.resident_limit {
            return Ok(BudgetCheckResult::ok(false));
        }
        if policy == OverflowPolicy::Truncate {
            let mut r = BudgetCheckResult::ok(false);
            r.detail = json!({"truncated": true, "resident_tokens": resident_tokens});
            return Ok(r);
        }
        Err(McpError::new(
            "MCP_BUDGET_EXCEEDED",
            json!({"required": resident_tokens, "available": self.caps.resident_limit, "suggestions": ["减小 X-MCP-Tool-Groups 后重取 tools/list"]}),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn constants_match_spec() {
        assert_eq!(RESIDENT_TOTAL_LIMIT, 2000);
        assert_eq!(MAX_RESULT_SINGLE, 1500);
        assert_eq!(MAX_RESULT_CUMULATIVE, 2500);
        assert_eq!(MAX_SCHEMA_COUNT, 12);
        assert_eq!(PEAK_LIMIT, 7500);
    }
    #[test]
    fn estimate_at_least_one() {
        assert_eq!(estimate_tokens(&json!({})), 1);
    }
    #[test]
    fn resident_fail_vs_truncate() {
        let l = BudgetLedger::new(None);
        assert_eq!(
            l.check_resident(2500, OverflowPolicy::Fail)
                .unwrap_err()
                .code,
            "MCP_BUDGET_EXCEEDED"
        );
        assert!(
            l.check_resident(2500, OverflowPolicy::Truncate)
                .unwrap()
                .allowed
        );
    }
    #[test]
    fn summary_mode_after_cumulative() {
        let mut l = BudgetLedger::new(None);
        l.record_result("s", 2500);
        assert!(l.check_result("s", 10).summary);
    }
}
