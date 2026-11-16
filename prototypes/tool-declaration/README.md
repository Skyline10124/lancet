# 工具声明原型

核对 [ADR 0002](https://github.com/Skyline10124/lancet/blob/2850986cd85544c17106960766fcbd3c757359e0/docs/adr/0002-lancet-node-api-tools-and-act.md) 第 2 节“声明由组件的 WIT 接口与 effect 清单组成”能否落地。结论见 [REPORT.md](REPORT.md)。

## 目录

| 路径 | 内容 |
| --- | --- |
| `wit/lancet-host/host.wit` | 节点的宿主接口 `lancet:host/handles`：四种句柄各是一种资源类型 |
| `tools/notes/` | 第三方示例工具：WIT 世界、effect 清单 `effects.json`、Rust 实现（含一个伪造句柄的恶意函数） |
| `tools/stale-host/` | 按与节点不一致的旧宿主接口编译的工具 |
| `node/` | 节点侧：打包自定义段、解出接口、核对清单、渲染 `.pyi`、构造 Starlark 类型、在 `wasmtime` 中实例化与调用 |
| `run.sh` | 构建全部变体并运行核对 |
| `results/` | 核对结果 `suite.json`、两份 `.pyi`、组件的 WIT 与自定义段清单、产物摘要 |

## 运行

```bash
./run.sh
```

需要 `rustc 1.96.0` 的 `wasm32-unknown-unknown` 与 `wasm32-wasip2` 目标，以及 `wasm-tools`（本次为 1.254.0）。构建产物写入 `target/artifacts/`，不入库。
