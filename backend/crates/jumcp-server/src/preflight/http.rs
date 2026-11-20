//! 接口族的参数判定：第三方 HTTP/接口工具在真正发出请求前先看"打到哪、要多少"。
//!
//! 三类风险各自独立：
//! - **目标**：URL 指向回环/链路本地/云元数据地址（服务端请求伪造的经典落点），或内网地址
//!   （`allow_private_hosts` 控制）；非 http(s) 协议与 URL 内嵌凭证同样拒绝。
//! - **量级**：分页大小超上限、时间跨度超上限——它们让"一次调用"变成整库导出。
//! - **参数注入**：字符串参数里出现 SQL 注入片段（`'; DROP`、`UNION SELECT`、`OR 1=1`）。
//!
//! | rule | 触发 |
//! |---|---|
//! | `http_empty` | URL 键存在但值为空白 |
//! | `http_scheme` | URL 协议不是 `http` / `https` |
//! | `http_embedded_credentials` | URL 带 userinfo（`user:pass@host`） |
//! | `http_private_target` | 目标为回环/链路本地/元数据/内网地址 |
//! | `http_page_size_too_large` | 分页大小超过 `max_page_size` |
//! | `http_time_range_too_wide` | 时间范围跨度超过 `max_time_range_days` |
//! | `http_param_injection` | 字符串参数含注入片段 |
//!
//! 顺序即优先级：先判目标（SSRF 最危险），再判量级，最后判注入。

use super::{Family, PreflightConfig, Rejection, reject};
use serde_json::Value;

/// 代表接口地址的入参键。
const URL_KEYS: &[&str] = &["url", "uri", "endpoint", "target_url", "webhook"];

/// 代表分页大小的入参键（大小写两种常见写法都认）。
const PAGE_SIZE_KEYS: &[&str] = &[
    "page_size",
    "pageSize",
    "pagesize",
    "per_page",
    "perPage",
    "perpage",
    "page_count",
    "pageCount",
    "batch_size",
    "batchSize",
    "rows",
    "top",
    "take",
    "limit",
];

/// 时间范围的成对键：两侧都能解析出时间才判跨度，解析不出就不猜。
const RANGE_PAIRS: &[(&str, &str)] = &[
    ("start_time", "end_time"),
    ("startTime", "endTime"),
    ("start_at", "end_at"),
    ("startAt", "endAt"),
    ("start", "end"),
    ("begin", "end"),
    ("from", "to"),
    ("since", "until"),
    ("date_from", "date_to"),
    ("start_date", "end_date"),
];

/// 云元数据服务的固定主机名：拿到它等于拿到实例凭据。
const METADATA_HOSTS: &[&str] = &[
    "metadata.google.internal",
    "metadata.goog",
    "instance-data",
    "metadata.azure.com",
];

pub(super) fn check(arguments: &Value, cfg: &PreflightConfig) -> Result<(), Rejection> {
    let Some(map) = arguments.as_object() else {
        return Ok(());
    };
    let urls: Vec<&str> = map
        .iter()
        .filter(|(key, _)| URL_KEYS.contains(&key.as_str()))
        .filter_map(|(_, value)| value.as_str())
        .collect();
    for url in &urls {
        if url.trim().is_empty() {
            return Err(reject(
                Family::Http,
                "http_empty",
                "接口地址为空：没有可请求的目标。",
                "传入完整的 http(s) 地址后重试。",
            ));
        }
        let Some(parts) = parse_url(url) else {
            // 不是标准 `scheme://authority` 形状：交给上游自己解释，本层不猜。
            continue;
        };
        if parts.scheme != "http" && parts.scheme != "https" {
            return Err(reject(
                Family::Http,
                "http_scheme",
                format!("只允许 http/https 接口地址，当前是 {}。", parts.scheme),
                "改用 http(s) 地址后重试。",
            ));
        }
        if parts.has_userinfo {
            return Err(reject(
                Family::Http,
                "http_embedded_credentials",
                "接口地址内嵌了凭证（user:pass@host）。",
                "把凭证放到上游自身的鉴权配置里，不要写进 URL。",
            ));
        }
        match host_class(&parts.host) {
            HostClass::Metadata | HostClass::Loopback => {
                return Err(reject(
                    Family::Http,
                    "http_private_target",
                    format!("禁止请求回环/链路本地/元数据地址：{}", parts.host),
                    "改为请求业务侧的公网或已授权内网地址。",
                ));
            }
            HostClass::Private if !cfg.allow_private_hosts => {
                return Err(reject(
                    Family::Http,
                    "http_private_target",
                    format!("禁止请求内网地址：{}", parts.host),
                    "如需访问内网接口，由管理员设置 JUMCP_HTTP_ALLOW_PRIVATE_HOSTS=1。",
                ));
            }
            _ => {}
        }
    }
    if let Some((key, value)) = map
        .iter()
        .filter(|(key, _)| PAGE_SIZE_KEYS.contains(&key.as_str()))
        .find_map(|(key, value)| numeric(value).map(|value| (key, value)))
        && value > cfg.max_page_size
    {
        return Err(reject(
            Family::Http,
            "http_page_size_too_large",
            format!("分页大小 {value} 超过单次上限 {}。", cfg.max_page_size),
            format!("把 {key} 改为 ≤{} 并分页取数后重试。", cfg.max_page_size),
        ));
    }
    for (start_key, end_key) in RANGE_PAIRS {
        let (Some(start), Some(end)) = (
            map.get(*start_key).and_then(time_point),
            map.get(*end_key).and_then(time_point),
        ) else {
            continue;
        };
        let span_secs = end - start;
        if span_secs > cfg.max_time_range_days * 86_400 {
            return Err(reject(
                Family::Http,
                "http_time_range_too_wide",
                format!(
                    "时间范围跨度过大（{start_key}→{end_key} 超过 {} 天）。",
                    cfg.max_time_range_days
                ),
                format!(
                    "把范围缩到 ≤{} 天，或按天分批调用后重试。",
                    cfg.max_time_range_days
                ),
            ));
        }
    }
    for (key, value) in map {
        let Some(text) = value.as_str() else {
            continue;
        };
        if has_injection(text) {
            return Err(reject(
                Family::Http,
                "http_param_injection",
                format!("参数 {key} 含 SQL 注入片段。"),
                "参数按纯数据传入，不要拼接语句片段；需要查询请走 SQL 通道。",
            ));
        }
    }
    Ok(())
}

/// 解析后的 URL 关键部分：只取判定需要的三段。
struct UrlParts {
    scheme: String,
    host: String,
    has_userinfo: bool,
}

/// 拆 `scheme://authority/path`：协议、主机（去端口与 IPv6 方括号）、是否带 userinfo。
fn parse_url(url: &str) -> Option<UrlParts> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.trim().to_ascii_lowercase();
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let (userinfo, authority) = match authority.rsplit_once('@') {
        Some((userinfo, host)) => (Some(userinfo), host),
        None => (None, authority),
    };
    let host = if let Some(inner) = authority.strip_prefix('[') {
        inner.split(']').next().unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    Some(UrlParts {
        scheme,
        host: host.trim().to_ascii_lowercase(),
        has_userinfo: userinfo.is_some_and(|value| !value.is_empty()),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostClass {
    Public,
    Private,
    Loopback,
    Metadata,
}

/// 主机分类：元数据/回环/链路本地永远拒绝，内网由配置放行。
fn host_class(host: &str) -> HostClass {
    if METADATA_HOSTS.contains(&host) {
        return HostClass::Metadata;
    }
    if host == "localhost" || host.ends_with(".localhost") {
        return HostClass::Loopback;
    }
    if let Some(quad) = host.rsplit(':').next().filter(|tail| tail.contains('.')) {
        return ipv4_class(quad);
    }
    if host.contains(':') {
        return ipv6_class(host);
    }
    ipv4_class(host)
}

fn ipv4_class(host: &str) -> HostClass {
    let octets: Vec<u8> = host
        .split('.')
        .map(|part| part.parse::<u8>().ok())
        .collect::<Option<Vec<u8>>>()
        .unwrap_or_default();
    let [a, b, c, d] = octets[..] else {
        return HostClass::Public;
    };
    match (a, b, c, d) {
        (127, ..) | (0, 0, 0, 0) => HostClass::Loopback,
        (169, 254, ..) => HostClass::Metadata,
        (10, ..) | (192, 168, ..) | (172, 16..=31, ..) | (100, 64..=127, ..) => HostClass::Private,
        _ => HostClass::Public,
    }
}

fn ipv6_class(host: &str) -> HostClass {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host == "::1" || host == "::" {
        return HostClass::Loopback;
    }
    if host.starts_with("fe80") {
        return HostClass::Metadata;
    }
    if host.starts_with("fc") || host.starts_with("fd") || host.starts_with("ff") {
        return HostClass::Private;
    }
    HostClass::Public
}

/// 数值兼容：JSON 数字或数字字符串（表单/查询串常把数字传成字符串）。
fn numeric(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str()?.trim().parse::<i64>().ok())
}

/// 时间点解析：epoch 秒/毫秒、RFC3339、`YYYY-MM-DD`、`YYYY-MM-DD HH:MM:SS`（UTC）。
///
/// 只用于跨度判定；解析不出就跳过这条规则——猜测时间格式会把合法调用误杀。
fn time_point(value: &Value) -> Option<i64> {
    if let Some(raw) = value.as_i64() {
        return Some(epoch_seconds(raw));
    }
    let text = value.as_str()?.trim();
    if let Ok(raw) = text.parse::<i64>() {
        return Some(epoch_seconds(raw));
    }
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(parsed.timestamp());
    }
    if let Ok(parsed) = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S") {
        return Some(parsed.and_utc().timestamp());
    }
    chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .map(|date| {
            date.and_hms_opt(0, 0, 0)
                .expect("00:00 合法")
                .and_utc()
                .timestamp()
        })
}

/// epoch 秒与毫秒都接受：13 位以上按毫秒处理。
fn epoch_seconds(raw: i64) -> i64 {
    if raw.abs() >= 100_000_000_000 {
        raw / 1000
    } else {
        raw
    }
}

/// 注入片段：都是"只有拼接语句才会出现"的形状，纯数据里出现同样应当拒绝。
fn has_injection(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    if lowered.contains("union select") {
        return true;
    }
    if lowered.contains(" or 1=1") || lowered.contains("' or '") || lowered.contains("\" or \"") {
        return true;
    }
    if lowered.contains("sleep(") || lowered.contains("benchmark(") {
        return true;
    }
    // `;` 后跟写语句：允许空白，`;drop` 与 `; drop` 都算。
    let mut rest = lowered.as_str();
    while let Some(index) = rest.find(';') {
        rest = rest[index + 1..].trim_start();
        if ["drop", "delete", "insert", "update", "alter", "truncate"]
            .iter()
            .any(|keyword| rest.starts_with(keyword))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn check_args(arguments: Value) -> Result<(), Rejection> {
        check(&arguments, &PreflightConfig::default())
    }

    #[test]
    fn metadata_and_loopback_targets_are_rejected() {
        for url in [
            "http://169.254.169.254/latest/meta-data/",
            "http://127.0.0.1:8080/admin",
            "http://localhost/api",
            "http://[::1]/api",
            "http://metadata.google.internal/computeMetadata/v1/",
            "http://0.0.0.0/",
        ] {
            assert_eq!(
                check_args(json!({"url": url})).expect_err(url).rule,
                "http_private_target",
                "{url}"
            );
        }
    }

    #[test]
    fn private_targets_follow_the_config_switch() {
        assert_eq!(
            check_args(json!({"url": "http://192.168.12.81:8123/"}))
                .expect_err("默认拒绝")
                .rule,
            "http_private_target"
        );
        let cfg = PreflightConfig {
            allow_private_hosts: true,
            ..PreflightConfig::default()
        };
        assert_eq!(
            check(&json!({"url": "http://192.168.12.81:8123/"}), &cfg),
            Ok(())
        );
        // 内网开关不放行元数据地址。
        assert!(check(&json!({"url": "http://169.254.169.254/"}), &cfg).is_err());
    }

    #[test]
    fn schemes_and_embedded_credentials_are_rejected() {
        assert_eq!(
            check_args(json!({"url": "file:///etc/passwd"}))
                .expect_err("协议")
                .rule,
            "http_scheme"
        );
        assert_eq!(
            check_args(json!({"endpoint": "https://user:pass@example.com/a"}))
                .expect_err("凭证")
                .rule,
            "http_embedded_credentials"
        );
    }

    #[test]
    fn oversized_page_and_time_range_are_rejected() {
        assert_eq!(
            check_args(json!({"url": "https://example.com/a", "page_size": 100000}))
                .expect_err("分页")
                .rule,
            "http_page_size_too_large"
        );
        assert_eq!(
            check_args(json!({"url": "https://example.com/a", "limit": "5000"}))
                .expect_err("字符串数字")
                .rule,
            "http_page_size_too_large"
        );
        assert_eq!(
            check_args(json!({
                "url": "https://example.com/a",
                "start_time": "2015-01-01",
                "end_time": "2025-01-01"
            }))
            .expect_err("跨度")
            .rule,
            "http_time_range_too_wide"
        );
        // 一天的范围放行。
        assert_eq!(
            check_args(json!({
                "url": "https://example.com/a",
                "start_time": "2025-01-01",
                "end_time": "2025-01-02"
            })),
            Ok(())
        );
    }

    #[test]
    fn injection_fragments_are_rejected() {
        for arguments in [
            json!({"url": "https://example.com/a", "q": "x' OR 1=1 --"}),
            json!({"url": "https://example.com/a", "q": "1; DROP TABLE t"}),
            json!({"url": "https://example.com/a", "q": "1' UNION SELECT password FROM users"}),
        ] {
            assert_eq!(
                check_args(arguments.clone()).expect_err("注入").rule,
                "http_param_injection",
                "{arguments}"
            );
        }
        assert_eq!(
            check_args(json!({"url": "https://example.com/a", "q": "正常检索词"})),
            Ok(())
        );
    }

    /// 非接口调用（没有 URL/分页/时间范围参数）不受影响；普通文本里的引号不是注入。
    #[test]
    fn plain_arguments_are_untouched() {
        for arguments in [
            json!({"table": "t", "filter": {"a": 1}}),
            json!({"q": "O'Brien"}),
            json!({"sql": "SELECT 1"}),
        ] {
            assert_eq!(check_args(arguments.clone()), Ok(()), "{arguments}");
        }
    }
}
