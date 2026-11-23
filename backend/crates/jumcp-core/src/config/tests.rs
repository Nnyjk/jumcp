use super::*;

use std::collections::HashSet;

#[test]
fn rejects_over_limit_groups() {
    let p = TenantPolicy {
        groups: (0..21)
            .map(|i| GroupDef {
                code: format!("db.read.g{i}"),
                name: format!("组{i}"),
                priority: 1,
                min_budget: 800,
            })
            .collect(),
        ..Default::default()
    };
    assert!(p.validate().is_err());
}
#[test]
fn rejects_unknown_role_group() {
    let mut roles = std::collections::HashMap::new();
    roles.insert(
        "analyst".into(),
        RoleDef {
            name: "analyst".into(),
            default_groups: vec!["db.read.nope".into()],
            default_budget: 2000,
        },
    );
    let p = TenantPolicy {
        groups: vec![GroupDef {
            code: "db.read.core".into(),
            name: "核心数据只读".into(),
            priority: 1,
            min_budget: 800,
        }],
        roles,
        ..Default::default()
    };
    assert!(p.validate().is_err());
}
#[test]
fn basic_is_builtin_and_cannot_be_defined_in_policy() {
    assert!(is_reserved_group_name("basic"));
    assert!(is_reserved_group_name("builtin"));
    assert!(is_reserved_group_name("admin"));
    for code in ["basic", "builtin"] {
        let p = TenantPolicy {
            groups: vec![GroupDef {
                code: code.into(),
                name: "内置组".into(),
                priority: 1,
                min_budget: 800,
            }],
            ..Default::default()
        };
        let err = p.validate().expect_err("内置组名不得出现在配置里");
        assert!(err.contains(code), "错误信息必须点名 {code}: {err}");
    }
    // admin 仍允许作为配置组（它是维护通道的授权组）。
    let p = TenantPolicy {
        groups: vec![GroupDef {
            code: "admin".into(),
            name: "管理员".into(),
            priority: 1,
            min_budget: 800,
        }],
        ..Default::default()
    };
    assert!(p.validate().is_ok());
}
#[test]
fn intersect_gate() {
    let s = Settings {
        groups_mode: "intersect".to_string(),
        ..Default::default()
    };
    assert!(s.validate().is_err());
    let s2 = Settings {
        groups_mode: "intersect".to_string(),
        policy: TenantPolicy {
            allow_intersect: true,
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(s2.validate().is_ok());
}
#[test]
fn budget_overflow_closed_set() {
    let s = Settings {
        budget_overflow: "sometimes".to_string(),
        ..Default::default()
    };
    assert!(s.validate().is_err());
    let ok = Settings {
        budget_overflow: "truncate".to_string(),
        ..Default::default()
    };
    assert!(ok.validate().is_ok());
    assert_eq!(
        ok.overflow_policy(),
        crate::budget::OverflowPolicy::Truncate
    );
}
#[test]
fn agent_tokens_parse_and_authenticate() {
    let tokens = AgentTokens::parse(
        "analyst-1:analyst:tok_a_000000000000, bot-admin:admin:tok_b_000000000000",
    )
    .unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(
        tokens
            .authenticate("tok_b_000000000000")
            .map(|p| (p.id.as_str(), p.role.as_str())),
        Some(("bot-admin", "admin")),
        "合法 token 必须能查到 agent_id 与 role"
    );
    assert!(tokens.authenticate("tok_unknown_000000").is_none());
    assert!(tokens.authenticate("").is_none());
    assert!(AgentTokens::parse("").unwrap().is_empty());
}

#[test]
fn agent_tokens_reject_bad_inputs() {
    assert!(
        AgentTokens::parse("analyst-1:analyst").is_err(),
        "缺少 token"
    );
    assert!(
        AgentTokens::parse("analyst-1::tok_000000000000").is_err(),
        "role 为空"
    );
    assert!(
        AgentTokens::parse("analyst-1:analyst:").is_err(),
        "token 为空"
    );
    assert!(
        AgentTokens::parse("A1:analyst:tok_000000000000").is_err(),
        "agent_id 非法"
    );
    assert!(
        AgentTokens::parse("a1:analyst:tok_1_00000000000,a1:analyst:tok_2_00000000000").is_err(),
        "agent_id 重复"
    );
    assert!(
        AgentTokens::parse("a1:analyst:tok_same_0000,a2:admin:tok_same_0000").is_err(),
        "token 重复"
    );
}

/// 角色必须在 policy.roles 里存在；否则启动期 Err，而不是每个请求才报未知角色。
#[test]
fn agent_token_role_must_exist_in_policy() {
    let mut roles = std::collections::HashMap::new();
    roles.insert(
        "analyst".into(),
        RoleDef {
            name: "analyst".into(),
            default_groups: vec!["db.read.core".into()],
            default_budget: 2000,
        },
    );
    let base = Settings {
        policy: TenantPolicy {
            groups: vec![GroupDef {
                code: "db.read.core".into(),
                name: "核心数据只读".into(),
                priority: 1,
                min_budget: 800,
            }],
            roles,
            ..Default::default()
        },
        agent_tokens: AgentTokens::parse("analyst-1:analyst:tok_000000000000").unwrap(),
        ..Default::default()
    };
    assert!(base.validate().is_ok());

    let bad = Settings {
        agent_tokens: AgentTokens::parse("ghost-1:ghost:tok_000000000000").unwrap(),
        ..base.clone()
    };
    let e = bad.validate().unwrap_err();
    assert!(e.contains("未定义角色"), "{e}");
}

#[test]
fn constant_time_eq_matches_semantics() {
    assert!(constant_time_eq("abc", "abc"));
    assert!(!constant_time_eq("abc", "abd"));
    assert!(!constant_time_eq("abc", "abcd"));
    assert!(!constant_time_eq("abcd", "abc"));
    assert!(constant_time_eq("", ""));
}

#[test]
fn loads_repo_policy_file() {
    let s = Settings::from_env().unwrap();
    assert!(s.validate().is_ok());
}

/// 新形状：组 code + 中文展示名；角色的 name 是展示名而不是 code。
#[test]
fn parses_group_code_with_chinese_names() {
    let text = r#"
tenant_budget_cap = 2000

[roles.analyst]
name = "数据分析"
default_groups = ["db.read.core"]
default_budget = 2000

[[groups]]
code = "db.read.core"
name = "核心数据只读"
priority = 10
min_budget = 800
"#;
    let p: TenantPolicy = toml::from_str(text).unwrap();
    assert_eq!(p.groups[0].code, "db.read.core");
    assert_eq!(p.groups[0].name, "核心数据只读");
    assert_eq!(p.groups[0].min_budget, 800);
    assert_eq!(p.roles["analyst"].name, "数据分析");
    assert_eq!(
        p.group_codes(),
        HashSet::from(["db.read.core".to_string()]),
        "内部组集合只含 code"
    );
    p.validate().unwrap();
}

/// 配置兼容：`[[groups]]` 只有 name 时，name 就是 code，展示名回退 code。
#[test]
fn legacy_name_only_group_keeps_code_as_display_name() {
    let text = r#"
[[groups]]
name = "db.read.core"
priority = 10
"#;
    let p: TenantPolicy = toml::from_str(text).unwrap();
    assert_eq!(p.groups[0].code, "db.read.core");
    assert_eq!(p.groups[0].name, "db.read.core");
    assert_eq!(p.groups[0].min_budget, 800);
}

/// 角色缺 name 时回退 code，不因展示名缺失启动失败。
#[test]
fn legacy_role_without_name_falls_back_to_code() {
    let text = r#"
[roles.executor]
default_groups = []
default_budget = 1600
"#;
    let p: TenantPolicy = toml::from_str(text).unwrap();
    assert_eq!(p.roles["executor"].name, "executor");
}

/// 展示名重复 code 是允许的（旧配置），但空展示名必须拒绝。
#[test]
fn empty_display_name_is_rejected() {
    let p = TenantPolicy {
        groups: vec![GroupDef {
            code: "db.read.core".into(),
            name: "  ".into(),
            priority: 1,
            min_budget: 800,
        }],
        ..Default::default()
    };
    assert!(p.validate().is_err());
}

/// 默认不托管：空串是合法值，行为与未引入静态托管前完全一致。
#[test]
fn empty_web_dir_is_valid_and_means_no_hosting() {
    assert!(validate_web_dir("").is_ok());
    let s = Settings::default();
    assert_eq!(s.web_dir, "");
    assert!(s.validate().is_ok());
}

/// 目录存在但产物没构建（缺 index.html）：启动期报错，而不是运行期 404。
#[test]
fn web_dir_without_index_html_is_rejected_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    let e = validate_web_dir(dir.path().to_str().unwrap()).unwrap_err();
    assert!(e.contains("缺少 index.html"), "{e}");

    let s = Settings {
        web_dir: dir.path().to_string_lossy().into_owned(),
        ..Default::default()
    };
    assert!(
        s.validate().is_err(),
        "缺 index.html 不得通过 Settings::validate"
    );
}

#[test]
fn web_dir_pointing_at_missing_path_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("dist");
    let e = validate_web_dir(missing.to_str().unwrap()).unwrap_err();
    assert!(e.contains("不是目录"), "{e}");
}

#[test]
fn web_dir_with_index_html_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "<!doctype html>").unwrap();
    validate_web_dir(dir.path().to_str().unwrap()).unwrap();
    let s = Settings {
        web_dir: dir.path().to_string_lossy().into_owned(),
        ..Default::default()
    };
    s.validate().unwrap();
}

/// HTTP 模式的双面端口：默认值、Env 接线之外，最关键的互斥校验。
fn http_settings(port: u16, admin_port: u16) -> Settings {
    Settings {
        transport: "streamable-http".into(),
        secret_key: Some(SecretKey::parse(&"11".repeat(32)).expect("secret key")),
        transport_key: Some(SecretKey::parse(&"22".repeat(32)).expect("transport key")),
        port,
        admin_port,
        ..Default::default()
    }
}

#[test]
fn admin_surface_defaults_are_login_ready() {
    let s = Settings::default();
    assert_eq!(s.admin_port, 8080, "管理面独立端口，默认 8080");
    assert_eq!(s.admin_session_ttl_secs, 43_200, "默认 12 小时");
    assert_eq!(
        s.static_audit_interval_secs, 300,
        "后台静态审计默认 5 分钟刷新一次"
    );
    assert!(
        !s.admin_cookie_secure,
        "默认允许内网 HTTP：Secure 会让浏览器不回传 cookie（登录后立刻 401）"
    );
}

#[test]
fn admin_port_must_differ_from_mcp_port() {
    let err = http_settings(8080, 8080)
        .validate()
        .expect_err("端口相同必须拒绝启动");
    assert!(
        err.contains("JUMCP_ADMIN_PORT") && err.contains("JUMCP_MCP_PORT"),
        "错误必须点名两个变量: {err}"
    );
    http_settings(8000, 8080)
        .validate()
        .expect("不同端口应当通过");
}

#[test]
fn admin_session_ttl_must_be_positive() {
    let mut s = http_settings(8000, 8080);
    s.admin_session_ttl_secs = 0;
    let err = s.validate().expect_err("TTL 必须为正数");
    assert!(err.contains("JUMCP_ADMIN_SESSION_TTL_SECS"), "{err}");
}

/// 两把密钥用途分离：相等等于共用一把，泄露一把连带失去两层保护。
#[test]
fn secret_and_transport_keys_must_differ() {
    let mut s = http_settings(8000, 8080);
    s.transport_key = s.secret_key.clone();
    let err = s.validate().expect_err("两把密钥相同必须拒绝启动");
    assert!(
        err.contains("JUMCP_SECRET_KEY")
            && err.contains("JUMCP_TRANSPORT_KEY")
            && err.contains("用途分离"),
        "错误必须点名两个变量并说明用途分离: {err}"
    );
    http_settings(8000, 8080)
        .validate()
        .expect("两把不同密钥应当通过");
}
