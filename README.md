# LANCET

LANCET 是用 Rust 编写的独立工具执行运行时 `lancet-node`，对外提供 `act`。

> **警告**：项目处于早期搭建与快速迭代阶段，接口、配置和实现仍会有破坏性调整，请勿用于生产环境。
>
> 本项目主要由 Code Agents 驱动，个人开发者实力有限，使用前风险自斟。

## 定位与状态

- **职责**：作为独立的工具执行运行时 `lancet-node`，向调用方提供 `act` 工具面，承接脚本化编排、组件隔离执行与调用回执。
- **当前状态**：仓库已完成治理文档与 `xtask` 工程基线初始化；`lancet-node` 与 `act` 的架构决策及业务 crate 将在后续 ADR 与合同中定稿并演进。

## 环境要求与验证

目前仅支持 Linux 主机。Rust 工具链版本由 [rust-toolchain.toml](rust-toolchain.toml) 固定。

在仓库根目录运行本地检查：

```sh
cargo xtask docs-check
cargo xtask ci
```

## 开发与贡献

任务、问题与实现讨论在 GitHub Issues 和 PR 中进行。
提交贡献或使用编码 Agent 前，请阅读 [AGENTS.md](AGENTS.md)。

## 文档导航

| 内容 | 入口 |
| --- | --- |
| 文档索引 | [docs/README.md](docs/README.md) |
| Agent 工作指引 | [AGENTS.md](AGENTS.md) |
| 工程规范 | [docs/development/engineering.md](docs/development/engineering.md) |
| 领域术语 | [docs/reference/glossary.md](docs/reference/glossary.md) |

## 许可证

本项目采用 [Apache-2.0](LICENSE) 许可证。
