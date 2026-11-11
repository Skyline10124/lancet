# 文档索引

本页按主题列出文档入口。文档的位置与写法见[文档写作](governance/writing.md)。

## 项目入口

- [项目介绍](../README.md)：项目定位、环境要求与本地验证。
- [Agent 工作指引](../AGENTS.md)：阅读顺序、推进任务的方式与交付要求。

## 治理

- [项目宪章](governance/charter.md)：角色、规则怎样生效，以及 Agent 的工作原则。
- [Agent 身份与权限](governance/agent-permissions.md)：bot 身份，Agent 与维护者各自执行的操作，安全边界与委派。
- [Git 与 GitHub](governance/git-operations.md)：操作流程，以及提交、Issue、PR 与评论的格式模板。
- [文档写作](governance/writing.md)：文档位置、语言与状态、写法，以及架构合同与 ADR 的模板。

## 开发与验证

- [工程规范](development/engineering.md)：设计中的归属、代码可读性、测试判定、TDD 与验证范围。

## 架构决策

- [ADR 0001](adr/0001-lancet-protocol.md)（proposed）：以 `lancet-protocol` 定义外部上下文节点协议（连接、零重放、五态结局、回执与求逆）。
- [ADR 0002](adr/0002-lancet-node-api-tools-and-act.md)（proposed）：`lancet-node` 以 Starlark 编排、API 工具、宿主资源与写时复制提供 `act`。

## 参考资料

- [领域术语](reference/glossary.md)：项目唯一术语表，保留英文名称、中文定义、易混淆称呼及合同章节入口。
- [Agent 辅助资料](agents/)：任务系统、标签映射与领域文档读取资料。

ADR 与合同的分工见[文档写作](governance/writing.md#文档放在哪里)。
