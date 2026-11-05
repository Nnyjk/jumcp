# 功能点：上下文预算的必要性与计量

问题：为什么必须给"常驻定义 + 注入 schema + 工具结果"设硬上限，以及怎么计。

## 依据

| 来源 | 结论 |
|---|---|
| Lost in the Middle（Liu et al., TACL 2024，arXiv 2307.03172） | 长上下文中信息位置影响显著：开头/结尾召回好，中段最差（U 形）。[arxiv.org/abs/2307.03172](https://arxiv.org/abs/2307.03172) |
| Chroma《Context Rot》（2025-07，18 个前沿模型） | 输入变长时输出质量非均匀下降，"塞得下"不等于"用得好"。[research.trychroma.com/context-rot](https://research.trychroma.com/context-rot) |
| Anthropic《Effective context engineering for AI agents》 | 上下文是有限资源，主张最小必要集合 + 渐进披露 + 工具结果精简。[anthropic.com/research/effective-context-engineering-for-ai-agents](https://www.anthropic.com/research/effective-context-engineering-for-ai-agents) |
| MCP 官方讨论 #2812 | 工具定义常驻是主要固定成本（123 tok/tool，中位 103）。[discussions/2812](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/2812) |

## 契合度与取舍

- 三份独立来源指向同一结论：**预算要按"会话内累计占用"控制，而不是单轮峰值**。这支持 §5.1 把 schema 注入改为累计 12 个、tool result 改为会话累计 2,500 的修改。
- 位置效应（U 形）说明：把最有用的信息（当前任务、错误语义、选中 schema）放在**靠近末尾**的位置，长期常驻的工具目录反而应最小化——支持"索引 + 按需注入"而不是"全量预载"。
- **计量口径**：不同模型 tokenizer 不同，跨 tokenizer 的估算只能做预算粗算；验收以 provider 的 token 计量接口/实测为准。因此文档中"±15% 余量"只作为**告警阈值的经验缓冲**，不作为度量依据。

## 对本计划的具体约束

- 预算三行（常驻 / 注入 schema / 结果）都必须"按会话累计"，告警与上限用实测 token 校准后再定稿（阶段〇基线）。
- 结果超限优先"摘要 + 分页引用"，其次"拒绝执行"，禁止静默截断（静默截断即静默降级）。
