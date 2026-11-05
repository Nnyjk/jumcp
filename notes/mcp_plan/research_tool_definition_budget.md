# 功能点：工具定义常驻成本

问题：MCP 的 `tools/list` 结果每轮进入上下文，工具数线性增长时成本不可控。

## 候选方案

| 方案 | 做法 | 实测数据 |
|---|---|---|
| Anthropic Code execution with MCP | 把工具暴露为文件系统里的代码 API，模型按需读取单个定义并写代码调用；中间结果不进上下文 | 上下文 token 15 万 → 2 千（−98.7%），来源：[anthropic.com/research/code-execution-with-mcp](https://www.anthropic.com/research/code-execution-with-mcp) |
| 工具设计层压缩（Anthropic《Writing effective tools for agents》） | 工具按前缀命名空间化（`asana_search` / `jira_search`）、合并同类工具、`response_format: concise/detailed` 让模型按需取精简结果（约占详细版 1/3 token） | 定性 + 案例，来源：[anthropic.com/research/writing-tools-for-agents](https://www.anthropic.com/research/writing-tools-for-agents) |
| 生态基线测量（MCP 官方讨论 #2812） | 逐 server 统计定义 token 数 | 全生态平均 123 tok/tool、中位 103；带完整枚举的重型工具 800–1,200。来源：[modelcontextprotocol/discussions/2812](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/2812) |

## 契合度与取舍

- **借鉴（Code execution）**：核心可迁移的是"定义按需加载 + 中间结果不进上下文"两条原则，不是代码执行沙箱本身。本服务是纯 MCP Server，没有代码执行环境，因此在协议内等价实现为 `search_capabilities`（返回一行摘要，不含 schema）→ `describe_capability`（按需注入完整 schema）→ §4.3.6 结果摘要。
- **采纳（工具设计层）**：命名空间化对应 §4.1.2 的 group 前缀；`response_format: concise` 对应结果摘要模式（`summary: true`）。
- **采纳（生态基线）**：§1.1 的量级改为引用 123 tok/tool（中位 103）+ 重型工具 800–1,200，替换原"26% → 1.6%"无出处数据。
- **放弃**：完全照搬 Code execution——需要客户端支持代码执行工具与沙箱，且与非 Anthropic 客户端不兼容；超出 MCP Server 的职责边界（见 §1.3 非目标）。

## 对本计划的具体约束

- 单工具定义预算按 **150–250 tok** 设计（生态中位 103 之上，留参数与描述余量）；超过 400 视为定义过重，须拆或改模板。
- 初始化常驻不再解释为"工具数量"问题，而是"任何进入 `tools/list` 的定义都按会话累计计费"（§5.3 计量口径）。
