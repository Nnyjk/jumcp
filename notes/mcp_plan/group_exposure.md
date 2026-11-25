# 组暴露矩阵与权限收敛

## 被否决的决定

- **上游配置带授权组（`mcp_upstreams.group_code`）**：一个上游同时服务多个组是常态，把组属性绑在上游上逼着运维为不同组复制同一台上游配置（连接、密钥、探测状态全重复）。上游只保留连接与开关属性，授权收归组维度。
- **令牌工具级白名单（`access_tokens.builtin_tools_json`）**：接入外部 MCP 后工具名由上游决定且随版本漂移，逐名白名单在管理上不可维护，同时与「组」形成两套重叠授权。令牌只保留 `groups`，工具级可见性统一在组内配置。
- **`basic` 作为隐式公共通道**：常驻元工具（`search_capabilities` / `describe_capability` / `list_groups` / `db_run_template`）是缺了就不能用的基础能力，但把 `basic` 做成不经授权即对所有请求开放的隐式通道等于绕过组门禁。改为普通可授权组（`grantable:true`），由令牌与角色显式勾选。
- **二元逐工具规则（只有加入/不加入）**：只有「加入」无法表达「已经把整台上游授给某组，但要从中摘掉个别高风险或不需要的工具」。第二层采用三态（无规则行 `inherit` 继承第一层、`include` 强制纳入、`exclude` 强制排除），规则优先于第一层。
- **存量迁移自动回填 `basic`**：权限系统里「静默放宽」比「静默收窄」更危险——收窄会在第一次调用时立刻暴露，放宽则会悄悄把原本被白名单拦住的常驻元工具放出去。旧令牌的 `builtin_tools_json` 没有等价物，升级不做自动补授权，由管理员按需勾选 `basic`。

## 约束

- **`TenantPolicy` 保留名处理**：`TenantPolicy::validate` 仍拒绝在配置里定义 `basic` 或 `builtin`，但 `group_codes()` 恒含 `basic`、`group("basic")` 返回内置定义（`name="基础工具"`、`priority=0`、`min_budget=800`），保证令牌与角色可以合法引用 `basic` 而不放开配置文件覆盖。
- **规则条数上限**：单组单上游逐工具规则上限 `GROUP_TOOL_RULE_MAX = 128`；`PUT /admin/groups/{code}/exposure` 单次提交变更上限 `512`（`GROUP_TOOL_RULE_MAX * 4`）。
- **整请求校验**：`PUT /admin/groups/{code}/exposure` 先校验整请求（上游存在性、工具是否在缓存、`admin` 类别或 `admin_` 前缀工具不得进非 `admin` 组、规则状态合法性）再落库，不做部分写入。
- **迁移时机与幂等**：`CapabilityStore::open` 内在任何读写之前执行迁移，只按旧表/旧列是否存在判断幂等，迁完立即 `DROP` 旧结构（`mcp_upstream_tool_groups`、`mcp_upstreams.group_code`、`idx_upstream_group`、`access_tokens.builtin_tools_json`），防止下次启动把已删除的授权重新灌回或留下双写歧义。
