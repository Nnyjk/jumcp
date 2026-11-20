//! 文件族的路径/访问类型判定：第三方文件工具在被调用前先看"要碰哪、怎么碰"。
//!
//! 方言未知同理——上游的路径语义由它自己定义，本层只认入参形状：路径键给出**目标**，
//! 访问类型键给出**动作**。两类判断都不需要读文件系统：
//!
//! | rule | 触发 |
//! |---|---|
//! | `file_empty` | 路径键存在但值为空白 |
//! | `file_write_access` | 访问类型是写/删除/改名/改权限，或 `write`/`overwrite` 为 true |
//! | `file_traversal` | 路径含 `..` 段 |
//! | `file_secret_path` | 路径指向凭证/密钥/系统账户文件 |
//! | `file_out_of_scope` | 绝对路径且不在 `allowed_roots` 内（白名单为空则跳过） |
//!
//! 顺序即优先级：写动作最该被拦（无论目标在哪），其次是穿越与敏感路径，最后才是根目录约束。

use super::{Family, PreflightConfig, Rejection, reject};
use serde_json::Value;

/// 代表文件路径的入参键。只认这些名字：上游自定义的其它键本层无权猜。
const PATH_KEYS: &[&str] = &[
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

/// 代表访问类型的入参键：值形如 `read` / `write` / `delete`。
const ACCESS_KEYS: &[&str] = &["access", "access_type", "mode", "operation", "op", "action"];

/// 非只读访问类型：命中即拒绝（本服务只承载只读取数）。
const WRITE_ACCESS: &[&str] = &[
    "write", "append", "create", "update", "delete", "remove", "unlink", "truncate", "chmod",
    "chown", "mkdir", "rmdir", "move", "rename", "copy", "upload", "put", "patch", "set",
];

/// 凭证/密钥/系统账户类路径片段（小写、`\` 已归一为 `/`）。
const SECRET_FRAGMENTS: &[&str] = &[
    "/.ssh/",
    "/.aws/",
    "/.kube/",
    "/.gnupg/",
    "/.docker/config.json",
    "/.netrc",
    "/.git-credentials",
    "/.npmrc",
    "/.pypirc",
    "/etc/passwd",
    "/etc/shadow",
    "/etc/sudoers",
    "/etc/ssh/",
    "/proc/",
    "/sys/",
    "/root/",
    "/windows/system32/",
];

/// 敏感文件名的精确/前缀匹配：私钥与 `.env` 系列。
const SECRET_FILE_NAMES: &[&str] = &["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"];
const SECRET_EXTENSIONS: &[&str] = &[".pem", ".pfx", ".p12"];

pub(super) fn check(arguments: &Value, cfg: &PreflightConfig) -> Result<(), Rejection> {
    let Some(map) = arguments.as_object() else {
        return Ok(());
    };
    let paths: Vec<&str> = map
        .iter()
        .filter(|(key, _)| PATH_KEYS.contains(&key.as_str()))
        .filter_map(|(_, value)| value.as_str())
        .collect();
    if paths.is_empty() {
        return Ok(());
    }
    let write_access = has_write_access(map);
    for path in &paths {
        if path.trim().is_empty() {
            return Err(reject(
                Family::File,
                "file_empty",
                "文件路径为空：没有可访问的目标。",
                "传入具体路径后重试。",
            ));
        }
    }
    if write_access {
        return Err(reject(
            Family::File,
            "file_write_access",
            "文件工具只允许只读访问，当前调用声明了写/删除类的访问类型。",
            "改为只读访问（access=read）或走其它通道。",
        ));
    }
    for path in &paths {
        let normalized = normalize(path);
        if has_parent_segment(&normalized) {
            return Err(reject(
                Family::File,
                "file_traversal",
                format!("路径含 `..` 段：{path}"),
                "改用不含 `..` 的规范化路径后重试。",
            ));
        }
        if is_secret_path(&normalized) {
            return Err(reject(
                Family::File,
                "file_secret_path",
                format!("禁止访问凭证/系统账户类路径：{path}"),
                "只访问业务数据目录下的文件，不要读取密钥、`.env` 或系统账户文件。",
            ));
        }
    }
    if !cfg.allowed_roots.is_empty() {
        for path in &paths {
            let normalized = normalize(path);
            if is_absolute(&normalized) && !under_any_root(&normalized, &cfg.allowed_roots) {
                return Err(reject(
                    Family::File,
                    "file_out_of_scope",
                    format!("路径不在允许的根目录内：{path}"),
                    format!(
                        "只访问允许的目录（{}），或联系管理员加入 JUMCP_FILE_ALLOWED_ROOTS。",
                        cfg.allowed_roots.join(", ")
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// 访问类型判定：显式访问键命中写集合，或 `write`/`overwrite` 布尔为 true。
fn has_write_access(map: &serde_json::Map<String, Value>) -> bool {
    for (key, value) in map {
        if ACCESS_KEYS.contains(&key.as_str())
            && let Some(raw) = value.as_str()
            && WRITE_ACCESS.contains(&raw.trim().to_ascii_lowercase().as_str())
        {
            return true;
        }
        if matches!(key.as_str(), "write" | "overwrite") && value.as_bool() == Some(true) {
            return true;
        }
    }
    false
}

/// 路径归一：`\` → `/`、小写（Windows 盘符与大小写不敏感文件系统同一口径）。
fn normalize(path: &str) -> String {
    path.trim().replace('\\', "/").to_ascii_lowercase()
}

fn has_parent_segment(path: &str) -> bool {
    path.split('/').any(|segment| segment == "..")
}

fn is_absolute(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with("~/")
        || path == "~"
        // `c:/...`：盘符形式。
        || path
            .as_bytes()
            .first()
            .is_some_and(|first| first.is_ascii_alphabetic())
            && path.as_bytes().get(1) == Some(&b':')
}

fn is_secret_path(path: &str) -> bool {
    if SECRET_FRAGMENTS.iter().any(|fragment| path.contains(fragment)) {
        return true;
    }
    let file_name = path.rsplit('/').next().unwrap_or(path);
    if file_name == ".env" || file_name.starts_with(".env.") {
        return true;
    }
    if SECRET_FILE_NAMES
        .iter()
        .any(|name| file_name == *name || file_name.starts_with(&format!("{name}.")))
    {
        return true;
    }
    SECRET_EXTENSIONS
        .iter()
        .any(|extension| file_name.ends_with(extension))
}

/// 根目录前缀匹配必须落在路径段边界上：`/data` 不能放行 `/database/secret`。
fn under_any_root(path: &str, roots: &[String]) -> bool {
    roots.iter().any(|root| {
        let root = normalize(root);
        let root = root.trim_end_matches('/');
        path == root || path.starts_with(&format!("{root}/"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn check_args(arguments: Value) -> Result<(), Rejection> {
        check(&arguments, &PreflightConfig::default())
    }

    #[test]
    fn write_access_is_rejected_before_path_rules() {
        let err = check_args(json!({"path": "../../etc/passwd", "access": "delete"}))
            .expect_err("写访问必须拒绝");
        assert_eq!(err.rule, "file_write_access");
        let err = check_args(json!({"path": "/tmp/a.csv", "overwrite": true}))
            .expect_err("覆盖写必须拒绝");
        assert_eq!(err.rule, "file_write_access");
        assert_eq!(
            check_args(json!({"path": "/data/a.csv", "access": "read"})),
            Ok(())
        );
    }

    #[test]
    fn traversal_and_secret_paths_are_rejected() {
        assert_eq!(
            check_args(json!({"file": "data/../../etc/passwd"}))
                .expect_err("穿越")
                .rule,
            "file_traversal"
        );
        for path in [
            "~/.ssh/id_rsa",
            "/home/u/.aws/credentials",
            "/etc/passwd",
            "/srv/app/.env",
            "/srv/app/server.pem",
            "C:\\Windows\\System32\\config\\SAM",
        ] {
            assert_eq!(
                check_args(json!({"path": path}))
                    .expect_err(path)
                    .rule,
                "file_secret_path",
                "{path}"
            );
        }
    }

    #[test]
    fn root_allowlist_matches_on_segment_boundary() {
        let cfg = PreflightConfig {
            allowed_roots: vec!["/data".into(), "C:\\exports".into()],
            ..PreflightConfig::default()
        };
        assert_eq!(check(&json!({"path": "/data/a/b.csv"}), &cfg), Ok(()));
        assert_eq!(check(&json!({"path": "C:/exports/x.csv"}), &cfg), Ok(()));
        assert_eq!(check(&json!({"path": "/database/secret.csv"}), &cfg).expect_err("越界").rule, "file_out_of_scope");
        // 相对路径不受根目录约束（上游按自己的根解释）。
        assert_eq!(check(&json!({"path": "reports/a.csv"}), &cfg), Ok(()));
    }

    #[test]
    fn non_path_arguments_are_not_file_calls() {
        assert_eq!(check_args(json!({"table": "t", "filter": {"a": 1}})), Ok(()));
        assert_eq!(check_args(json!({"path": 5})), Ok(()));
    }
}
