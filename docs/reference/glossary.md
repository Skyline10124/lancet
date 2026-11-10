---
status: active
---

# LANCET

LANCET 是独立工具执行运行时 `lancet-node`，对外提供 `act` 工具面。
本文件是项目唯一的领域术语表，提供简短定义、关键区别与合同入口；完整规则由对应合同维护。
术语表不批准新语义。

## 术语

**lancet-node**:
独立的工具执行运行时进程，对外提供 `act` 工具面，承接调用请求、执行边界与回执交付。
见 [README.md](../../README.md#定位与状态)。
_Avoid_: in-process LancetRuntime, CEP host, embedded tool runner

**act**:
`lancet-node` 对外提供的单一工具面；具体协议、脚本模型与回执结构随架构合同与 ADR 0001 定稿后补入。
见 [README.md](../../README.md#定位与状态)。
_Avoid_: multi-tool CEP catalog, raw shell tool, untyped command string
