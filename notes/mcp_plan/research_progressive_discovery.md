# 功能点：渐进发现（search → describe → call）

问题：能力数量远超常驻预算，需要"先检索、再取定义"的两段式发现。

## 候选方案

| 方案 | 做法 | 结论数据 |
|---|---|---|
| RAG-MCP（arXiv 2505.03275） | 工具选择视为检索问题：MCP 工具元数据建外部向量索引，小模型编码任务后检索 top-k，只把命中项放进主模型 prompt | prompt token 降低 50%+，工具选择准确率提升 3 倍以上（官方摘要口径）。[arxiv.org/abs/2505.03275](https://arxiv.org/abs/2505.03275) |
| Toolshed / Advanced RAG-Tool Fusion（arXiv 2410.14594） | 把 agent 的即时工具集与全量工具域解耦；先召回候选工具集再逐子任务决策，无需微调 | 工具池扩至数百至数千时检索准确率不显著下降，且无需微调模型或 embedder。[arxiv.org/abs/2410.14594](https://arxiv.org/abs/2410.14594) |
| Anthropic Tool Search Tool（`defer_loading`） | 客户端/API 侧能力：全量目录交给 API，标记 `defer_loading: true` 的工具不预载，模型搜索后再展开定义 | 官方定位支持 10,000+ 工具。[anthropic.com/research/advanced-tool-use](https://www.anthropic.com/research/advanced-tool-use)；第三方接入说明 [litellm 文档](https://docs.litellm.ai/docs/providers/anthropic_tool_search) |
| Anthropic Agent Skills（两级渐进披露） | 第一层只常驻"何时该用"的元数据（数十 token），第二层正文按需读入 | 官方描述即为 progressive disclosure。[anthropic.com/research/equipping-agents-for-the-real-world-with-agent-skills](https://www.anthropic.com/research/equipping-agents-for-the-real-world-with-agent-skills) |

## 契合度与取舍

- **采纳（RAG-MCP / Toolshed）**：`search_capabilities(query, kind, group, limit=5)` 返回**不含完整 schema** 的一行摘要，命中后由 `describe_capability` 取定义——与两篇论文的"检索 top-k + 只注入命中项"同构。准确率随工具池扩大而崩溃是这两篇的共同前提，也是本计划把"能力可见性"当第一性问题（而非工具数量）的理由。
- **借鉴（Agent Skills 分层）**：本计划的三层等价映射为：分组索引（元数据层，≤20 组 / 300 tok）→ `search_capabilities` 一行摘要 → `describe_capability` 完整 schema。§5.1 的"累计 12 个 schema"即第二层预算。
- **放弃（依赖 `defer_loading`）**：这是 Anthropic API 侧的机制，非 MCP 协议能力；服务端无法假设客户端支持，故服务端自建检索面，并保留 §4.2.0 的客户端能力矩阵作为降级路径。
- **部分采纳（检索实现）**：两篇均以向量检索为主，但模板 id（`order.daily_summary`）与中文业务词强关键词可命中，故 §4.2.2 定为关键词 + 向量混合；`kind`/`group` 作为硬过滤先于打分，避免跨域误召回。

## 对本计划的具体约束

- `search_capabilities` 的 `limit` 默认 5（对齐 RAG-MCP 的 top-k 思路），上限 10；返回项禁止携带完整 schema。
- 检索必须**先按可见性硬过滤再打分**（论文前提是候选池即全集，本计划额外要求权限过滤前置，见 §4.2.3）。
