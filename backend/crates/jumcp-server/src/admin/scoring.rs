//! 定制化 tool 候选加权评分：把审计流水归纳成"哪张表 / 哪个接口值得固化成定制 tool"。
//!
//! 纯函数：输入事实、输出排序后的候选，不读时钟也不碰审计库——口径因此能被固定样本逐条锁住，
//! 且 HTTP 管理面与 MCP 内置工具共用同一份实现（同一批事实必然得出同一份排名）。
//!
//! 三条加权规则：
//!
//! | 规则 | 事实 | 权重 |
//! |---|---|---|
//! | 连续收紧 | 同令牌、同对象、短间隔的连续段，段内第 ⌈n/2⌉ 次之后 | 逐次递增 `k * REFINE_STEP` |
//! | 调用失败 | `outcome ∈ {error, unknown, budget_rejected}` | 每次 `-FAILURE_PENALTY` |
//! | 多令牌命中 | 同一对象被多个令牌各自访问 | `min((t-1) * MULTI_TOKEN_BONUS, MULTI_TOKEN_CAP)` |
//!
//! 分数越高的对象越该被提取成定制 tool：审计 agent 只需要看前几名就能决定下一轮做什么，
//! 不必逐条回放几十次调用去人肉判断"这个查询是不是被反复调参"。

use crate::preflight::table_refs;
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

/// 连续段的间隔上限（秒）：同令牌、同对象的相邻两跳超过它即断开成新的一段。
///
/// 取 10 分钟：一次"反复调参"发生在同一轮交付动作内，agent 相邻工具调用常是秒级；
/// 隔得更久的同对象调用通常是换了任务，缝进同一段只会造出一条假热点。
pub const SEGMENT_GAP_SECONDS: i64 = 600;

/// 连续段后半段的逐次加成步长：段长 n 时，第 ⌈n/2⌉+1 次起第 k 次加 `k * REFINE_STEP`。
///
/// 为什么递增而非常数：后一次尝试总是比前一次更接近目标形状（前一次的结果被看过、
/// 条件被改窄）；常数权重会让"试了 3 次就成"和"试了 9 次才成"同分，评审无从区分。
pub const REFINE_STEP: f64 = 1.5;

/// 失败调用的扣分：每次 -2.0。
///
/// 失败是"这条路走不通"的直接证据，权重必须压过短段收索引来的加分（段长 2 只有 1.5），
/// 否则反复报错的对象会冒充候选、把真正的热点挤出审计 agent 的观察范围。
pub const FAILURE_PENALTY: f64 = 2.0;

/// 多令牌加分的单位与上限：不同令牌各自访问同一对象，说明它是团队级公共需求，而不是
/// 某个 agent 一次性的探索。但"广度"证据弱于"深度"（连续收紧），故单位权重取 2.0 并封顶
/// 6.0——否则一个被十个令牌各碰一次的表会盖过被单个令牌反复收紧十次的接口。
pub const MULTI_TOKEN_BONUS: f64 = 2.0;
pub const MULTI_TOKEN_CAP: f64 = 6.0;

/// 聚合维度：决定 `subject` 的写法，也决定审计 agent 该如何落地成定制 tool。
///
/// `sql` / `file` / `http` 与前置拦截的三族同名，候选与拦截规则可以对照着看。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Family {
    Sql,
    File,
    Http,
    /// 认不出对象名时的退路：上游工具本身（`list_databases` 这类无对象参数的工具）。
    Tool,
}

impl Family {
    /// 候选响应里的族名（`sql` / `file` / `http` / `tool`）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sql => "sql",
            Self::File => "file",
            Self::Http => "http",
            Self::Tool => "tool",
        }
    }
}

/// 一条调用事实：一次调用落在一个对象上的证据。
#[derive(Debug, Clone)]
pub struct CallFact {
    /// 调用方令牌：优先 `actor.token_id`，缺省用 `actor.id`（同一身份的不同会话仍应累计）。
    pub token: String,
    /// 聚合键：SQL 表名（含库前缀）/ 文件路径 / 接口 `host+path` / 上游工具名。
    pub subject: String,
    pub family: Family,
    pub upstream_id: String,
    pub upstream_tool: String,
    /// `outcome ∈ {ok, summary}`：其余取值一律按失败计（成功口径与统计台账一致，
    /// 台账另把 `error` / `unknown` / `budget_rejected` 单列为 failure）。
    pub ok: bool,
    /// Unix 秒：段判定只需要相对先后与间隔，不关心展示格式。
    pub ts: i64,
}

/// 一条候选：分项证据全部带出来，审计 agent 不必回查原始流水就能解释排名。
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub subject: String,
    pub family: Family,
    pub upstream_id: String,
    pub upstream_tool: String,
    pub score: f64,
    pub calls: usize,
    /// 最长连续段长度（同令牌、间隔在 `SEGMENT_GAP_SECONDS` 内）。
    pub longest_segment: usize,
    /// 落在连续段后半段、因此被逐次加权的调用数。
    pub refined_calls: usize,
    pub failures: usize,
    pub distinct_tokens: usize,
    /// 加分/扣分理由，供审计 agent 直接引用。
    pub reasons: Vec<String>,
}

/// 一次调用的对象：SQL 按表逐条展开（JOIN 一拍多表），文件与接口各取键表里第一个非空值。
///
/// 族优先级与拦截层一致：入参里识别出 SQL 就只记 SQL，不再看路径/URL 键；文件与接口
/// 两类键可同时出现，因此一次调用可能同时产生两条事实（聚合粒度是证据，不是判定）。
///
/// 为什么 SQL 走 `preflight::table_refs`：拦截层已经在做同一件事，两处各写一套词法扫描
/// 必然漂移；评分只需要"稳定的聚合键"，复用它是唯一不会互相矛盾的来源。
pub fn subjects(arguments: &Value) -> Vec<(Family, String)> {
    let Some(map) = arguments.as_object() else {
        return Vec::new();
    };
    let mut out: Vec<(Family, String)> = Vec::new();
    if let Some(sql) = crate::preflight::sql_argument(arguments) {
        for table in table_refs(sql) {
            out.push((Family::Sql, table));
        }
        // SQL 命中后不再看文件/接口键：与 `preflight::check_call` 的族优先级一致，
        // 否则一条带 `path` 参数的 SQL 会被拆成两族候选，排名被平白稀释。
        return dedup(out);
    }
    if let Some(path) = first_string(map, FILE_ARGUMENT_KEYS) {
        out.push((Family::File, path.trim().replace('\\', "/")));
    }
    if let Some(subject) = first_string(map, URL_ARGUMENT_KEYS).and_then(url_subject) {
        out.push((Family::Http, subject));
    }
    dedup(out)
}

/// 入参里代表文件目标的键。与 `preflight::file` 的拦截键表是同一类概念但**各自演进**：
/// 这里只决定候选聚合粒度，漏认一个键的后果是少一条候选，没有安全含义。
const FILE_ARGUMENT_KEYS: &[&str] = &[
    "path",
    "file",
    "filepath",
    "file_path",
    "filename",
    "file_name",
    "dir",
    "directory",
    "target_path",
    "source_path",
];

/// 入参里代表接口地址的键（同上：只用于聚合）。
const URL_ARGUMENT_KEYS: &[&str] = &["url", "uri", "endpoint", "target_url", "webhook"];

fn first_string<'a>(map: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| map.get(*key).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
}

/// 接口聚合键取 `host+path`：查询串与请求体正是"条件收紧"发生的地方，
/// 把它们留在键里会让同一个接口的每次调参都变成一个新对象，规则一永远攒不出连续段。
fn url_subject(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let rest = rest.find(['?', '#']).map_or(rest, |end| &rest[..end]);
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
        .trim();
    if host.is_empty() {
        return None;
    }
    Some(format!("{}{path}", host.to_ascii_lowercase()))
}

fn dedup(mut subjects: Vec<(Family, String)>) -> Vec<(Family, String)> {
    subjects.sort();
    subjects.dedup();
    subjects
}

/// 按分数降序的候选列表；分数 ≤ 0 的对象不出现在结果里。
///
/// 为什么丢掉非正分：它们只说明"试过但没成/没成气候"，列出来既占满了 `limit`，
/// 又会让审计 agent 以为还有一个值得固化的对象。
pub fn score(facts: &[CallFact]) -> Vec<Candidate> {
    // 族名进键：同名表名与文件路径不该被合成一个对象（族决定落地成哪类定制 tool）。
    let mut groups: HashMap<(Family, &str, &str, &str), Vec<&CallFact>> = HashMap::new();
    for fact in facts {
        groups
            .entry((
                fact.family,
                fact.subject.as_str(),
                fact.upstream_id.as_str(),
                fact.upstream_tool.as_str(),
            ))
            .or_default()
            .push(fact);
    }
    let mut out: Vec<Candidate> = groups
        .into_iter()
        .filter_map(|((family, subject, upstream_id, upstream_tool), mut list)| {
            // 等 ts 时保持事实顺序（上游已按审计写入顺序排好），排序结果才可复现。
            list.sort_by_key(|fact| fact.ts);
            let mut by_token: HashMap<&str, Vec<i64>> = HashMap::new();
            for fact in &list {
                by_token
                    .entry(fact.token.as_str())
                    .or_default()
                    .push(fact.ts);
            }
            let mut refined_calls = 0usize;
            let mut longest_segment = 0usize;
            let mut points = 0.0;
            for times in by_token.values() {
                for segment in segment_lengths(times) {
                    let tail = segment - segment.div_ceil(2);
                    for step in 1..=tail {
                        points += REFINE_STEP * step as f64;
                    }
                    refined_calls += tail;
                    longest_segment = longest_segment.max(segment);
                }
            }
            let failures = list.iter().filter(|fact| !fact.ok).count();
            points -= FAILURE_PENALTY * failures as f64;
            let tokens: BTreeSet<&str> = list.iter().map(|fact| fact.token.as_str()).collect();
            let bonus = multi_token_bonus(tokens.len());
            points += bonus;
            let mut reasons: Vec<String> = Vec::new();
            if refined_calls > 0 {
                reasons.push(format!(
                    "同一令牌在 {} 分钟窗口内连续调用 {longest_segment} 次，后半段 {refined_calls} 次逐次加权",
                    SEGMENT_GAP_SECONDS / 60
                ));
            }
            if failures > 0 {
                reasons.push(format!(
                    "{failures} 次调用失败，扣 {:.1} 分",
                    FAILURE_PENALTY * failures as f64
                ));
            }
            if bonus > 0.0 {
                reasons.push(format!("{} 个不同令牌访问同一对象，加 {bonus:.1} 分", tokens.len()));
            }
            (points > 0.0).then(|| Candidate {
                subject: subject.to_string(),
                family,
                upstream_id: upstream_id.to_string(),
                upstream_tool: upstream_tool.to_string(),
                score: points,
                calls: list.len(),
                longest_segment,
                refined_calls,
                failures,
                distinct_tokens: tokens.len(),
                reasons,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| b.calls.cmp(&a.calls))
            .then_with(|| a.subject.cmp(&b.subject))
            .then_with(|| a.upstream_id.cmp(&b.upstream_id))
    });
    out
}

fn multi_token_bonus(tokens: usize) -> f64 {
    if tokens <= 1 {
        return 0.0;
    }
    (MULTI_TOKEN_BONUS * (tokens - 1) as f64).min(MULTI_TOKEN_CAP)
}

/// 时间升序序列按间隔切段，返回各段长度（段内相邻间隔 ≤ `SEGMENT_GAP_SECONDS`）。
fn segment_lengths(times: &[i64]) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    let mut current = 1usize;
    for pair in times.windows(2) {
        if pair[1] - pair[0] > SEGMENT_GAP_SECONDS {
            out.push(current);
            current = 1;
        } else {
            current += 1;
        }
    }
    if !times.is_empty() {
        out.push(current);
    }
    out
}
