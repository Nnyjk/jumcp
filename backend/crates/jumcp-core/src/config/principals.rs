//! 管理面与 agent 的主体令牌：token 派生身份，自述头不可信；比较一律常量时间。

/// 管理面主体：token 派生身份，`X-Admin-Actor` 只是自述（不可信）。
#[derive(Clone, PartialEq, Eq)]
pub struct AdminPrincipal {
    pub id: String,
    pub token: String,
}

/// token 是凭据：`{:?}` 只打 id，避免把明文写进日志。
impl std::fmt::Debug for AdminPrincipal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminPrincipal")
            .field("id", &self.id)
            .field("token", &"***")
            .finish()
    }
}

#[derive(Clone, Default)]
pub struct AdminTokens {
    principals: Vec<AdminPrincipal>,
}

impl std::fmt::Debug for AdminTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminTokens")
            .field("principals", &self.principals)
            .finish()
    }
}

impl AdminTokens {
    /// 解析 `id:token,id:token`。格式错误、id 重复、token 重复、token 过短一律拒绝（启动 fail-closed）。
    pub fn parse(raw: &str) -> Result<Self, String> {
        let mut principals: Vec<AdminPrincipal> = Vec::new();
        for item in raw.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let (id, token) = item
                .split_once(':')
                .ok_or_else(|| format!("JUMCP_ADMIN_TOKENS 项 {item:?} 必须是 id:token 形式"))?;
            let id = id.trim();
            let token = token.trim();
            if !is_principal_id(id) {
                return Err(format!(
                    "JUMCP_ADMIN_TOKENS 的 id {id:?} 非法（[a-z0-9._-]{{2,32}}）"
                ));
            }
            if token.len() < 16 {
                return Err(format!(
                    "JUMCP_ADMIN_TOKENS 的 token（{id}）必须至少 16 字符"
                ));
            }
            if principals.iter().any(|p| p.id == id) {
                return Err(format!("JUMCP_ADMIN_TOKENS 存在重复 id: {id}"));
            }
            if principals.iter().any(|p| p.token == token) {
                return Err(format!("JUMCP_ADMIN_TOKENS 存在重复 token（{id}）"));
            }
            principals.push(AdminPrincipal {
                id: id.to_string(),
                token: token.to_string(),
            });
        }
        Ok(Self { principals })
    }

    pub fn is_empty(&self) -> bool {
        self.principals.is_empty()
    }

    pub fn len(&self) -> usize {
        self.principals.len()
    }

    pub fn ids(&self) -> Vec<&str> {
        self.principals.iter().map(|p| p.id.as_str()).collect()
    }

    /// 常量时间鉴权：遍历全部候选、等长填充逐字节比并累积，不做 `==` 早退。
    pub fn authenticate(&self, presented: &str) -> Option<&str> {
        let mut matched: Option<&str> = None;
        for principal in &self.principals {
            if constant_time_eq(&principal.token, presented) {
                matched = Some(principal.id.as_str());
            }
        }
        matched
    }
}

pub fn is_principal_id(id: &str) -> bool {
    (2..=32).contains(&id.len())
        && id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// MCP 面的 agent 主体：`JUMCP_AGENT_TOKENS="agent_id:role:token,..."` 的解析结果。
///
/// 为什么单独一组 token：HTTP `/mcp` 必须按请求派生身份（`Actor.actor_id` 即 agent_id），
/// 否则所有 agent 共用进程默认角色，审计无法追责，admin 组也无从区分。
#[derive(Clone, PartialEq, Eq)]
pub struct AgentPrincipal {
    pub id: String,
    pub role: String,
    pub token: String,
}

/// token 是凭据：`{:?}` 只打 id/role，避免把明文写进日志。
impl std::fmt::Debug for AgentPrincipal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentPrincipal")
            .field("id", &self.id)
            .field("role", &self.role)
            .field("token", &"***")
            .finish()
    }
}

#[derive(Clone, Default)]
pub struct AgentTokens {
    principals: Vec<AgentPrincipal>,
}

impl std::fmt::Debug for AgentTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentTokens")
            .field("principals", &self.principals)
            .finish()
    }
}

impl AgentTokens {
    /// 解析 `agent_id:role:token,...`：格式错误、id 非法/重复、token 为空/重复一律拒绝。
    ///
    /// role 是否在 `policy.roles` 里由 `Settings::validate` 判定——解析发生在读策略文件之前，
    /// 这里看不到策略；启动时两处都会跑，因此非法角色仍然在启动期暴露（fail-closed）。
    pub fn parse(raw: &str) -> Result<Self, String> {
        let mut principals: Vec<AgentPrincipal> = Vec::new();
        for item in raw.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let mut parts = item.splitn(3, ':');
            let (Some(id), Some(role), Some(token)) = (parts.next(), parts.next(), parts.next())
            else {
                return Err(format!(
                    "JUMCP_AGENT_TOKENS 项 {item:?} 必须是 agent_id:role:token 形式"
                ));
            };
            let id = id.trim();
            let role = role.trim();
            let token = token.trim();
            if !is_principal_id(id) {
                return Err(format!(
                    "JUMCP_AGENT_TOKENS 的 agent_id {id:?} 非法（[a-z0-9._-]{{2,32}}）"
                ));
            }
            if role.is_empty() {
                return Err(format!("JUMCP_AGENT_TOKENS 的 agent {id} 缺少 role"));
            }
            if token.is_empty() {
                return Err(format!("JUMCP_AGENT_TOKENS 的 token（{id}）不能为空"));
            }
            if principals.iter().any(|p| p.id == id) {
                return Err(format!("JUMCP_AGENT_TOKENS 存在重复 agent_id: {id}"));
            }
            if principals.iter().any(|p| p.token == token) {
                return Err(format!("JUMCP_AGENT_TOKENS 存在重复 token（{id}）"));
            }
            principals.push(AgentPrincipal {
                id: id.to_string(),
                role: role.to_string(),
                token: token.to_string(),
            });
        }
        Ok(Self { principals })
    }

    pub fn is_empty(&self) -> bool {
        self.principals.is_empty()
    }

    pub fn len(&self) -> usize {
        self.principals.len()
    }

    pub fn principals(&self) -> &[AgentPrincipal] {
        &self.principals
    }

    /// 常量时间鉴权：遍历全部候选、等长填充逐字节比并累积，不做 `==` 早退。
    pub fn authenticate(&self, presented: &str) -> Option<&AgentPrincipal> {
        let mut matched: Option<&AgentPrincipal> = None;
        for principal in &self.principals {
            if constant_time_eq(&principal.token, presented) {
                matched = Some(principal);
            }
        }
        matched
    }
}

/// 等长填充比较：长度差与逐字节异或结果一起按位或，全程无早退。
pub fn constant_time_eq(expected: &str, presented: &str) -> bool {
    let a = expected.as_bytes();
    let b = presented.as_bytes();
    let len = a.len().max(b.len());
    let mut diff: u8 = (a.len() ^ b.len()).min(1) as u8;
    for i in 0..len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}
