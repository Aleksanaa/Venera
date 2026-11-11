# Venera 协议 1.0.0 设计研究

## 设计结论

建议采用 JSON-RPC 2.0 消息语义、OpenRPC 服务描述和 Schemars 数据 Schema 生成，沿用 VirtIO Serial 上的 JSONL 分帧。推荐以 Rust 数据类型和集中、强类型的方法声明作为唯一契约维护源，生成发布文档与各接收端的发现文档。

本篇记录候选设计及一手资料核对结果，供协议升级评审。当前运行协议与这份设计分别管理。

## 消息与传输

[JSON-RPC 2.0 §§1–6、8](https://www.jsonrpc.org/specification) 定义请求、结果、错误、通知和 batch，并明确协议与传输无关。同一实现可以同时担当 client 与 server，因此适合双向端点。

建议映射如下：

| 领域操作 | JSON-RPC 形态 |
|---|---|
| 查询状态、设置与恢复筛选、查询系统信息 | 有字符串 ID 的 request，以及同 ID 的 result 或 error |
| 日志记录、状态变更、日志缺口 | notification |
| 接口发现 | `rpc.discover` request |

Venera 的传输绑定可定义为每行一个完整 JSON-RPC 对象或 batch 数组，使用紧凑 UTF-8 JSON 与 LF 分隔。字符串内换行使用 JSON 转义。JSONL 是项目的传输绑定，JSON-RPC 提供消息语义。

标准对 batch 有明确行为：空数组是 Invalid Request；纯通知 batch 没有响应；混合 batch 只包含需要响应的条目，响应通过 ID 关联。建议首版一并实现这些标准行为，并用字面样本验收。

## 契约源与自动生成

推荐生成链为：

```text
Rust 领域类型 + 强类型方法声明
              ↓
     运行时分发与契约生成
              ├─ OpenRPC 文档及复用 Schema
              ├─ host / guest 接收接口发现文档
              ├─ 协议参考文档
              └─ 消费端类型
```

方法声明需要同时参与运行时注册和发现文档生成，才能确保方法名、请求类型和结果类型在同一处维护。Schemars 生成字段契约，方法行为与调用生命周期由 RPC 层及业务处理器表达。

[OpenRPC Introduction](https://spec.open-rpc.org/#introduction) 同时支持 code-first 和 design-first。另一可行方案是以 OpenRPC 文档为输入生成 Rust；[Typify 0.8.0](https://docs.rs/typify/0.8.0/typify/) 提供 JSON Schema 到 Rust 类型转换，但 OpenRPC 方法绑定及全部现有领域类型的覆盖仍需独立验证。

## 服务发现与版本

[OpenRPC Service Discovery Method](https://spec.open-rpc.org/#service-discovery-method) 定义 `rpc.discover`，返回服务的 OpenRPC 文档。建议双方将该调用作为稳定的启动入口，从实际方法清单获得可调用接口和数据契约。

一个契约维护源可以按接收角色生成两份文档：guest 描述查询与配置接口，host 描述接收事件的接口。各端返回自己的接收接口文档，符合 OpenRPC 的服务描述模型；共享领域类型由同一来源生成。

建议区分以下版本：

| 字段或标识 | 含义 |
|---|---|
| `jsonrpc: "2.0"` | 消息标准 |
| `openrpc` | 具体 OpenRPC 描述格式版本 |
| Schema dialect | 数据 Schema 的规范版本 |
| `info.version: "1.0.0"` | 与 Venera API 契约同步发布的文档版本 |
| 端点的软件版本 | 产品及二进制版本 |

[OpenRPC Versions、Openrpc Document 和 Info Object](https://spec.open-rpc.org/) 将 OpenRPC 格式版本、文档版本和实现版本分别定义。将 `info.version` 与 Venera 契约版本同步，是本项目建议采用的发布约定。主版本表达破坏性变更，次版本表达兼容扩展，补丁版本表达兼容修复。

上述发现描述已连接端点的接口。网络地址发现可作为独立的连接定位机制设计。

## Schema 方言

[OpenRPC Schema Object](https://spec.open-rpc.org/#schema-object) 要求使用 JSON Schema Draft-07。Schemars 1.2.3 默认方言为 2020-12，但提供 [`SchemaSettings::draft07()`](https://docs.rs/schemars/1.2.3/schemars/generate/struct.SchemaSettings.html)。其[固定版本源码](https://docs.rs/schemars/1.2.3/src/schemars/generate.rs.html#78-90) 配置相应的转换流程。

生成器应使用 Draft-07 设置及 `/components/schemas` 引用路径，统一收集定义并应用转换。请求参数按反序列化契约生成，返回结果按序列化契约生成。需要用真实领域样本验证 nullable、缺省、枚举、引用和字段约束的最终输出。

## 整数与领域状态

依据[整数契约研究与独立实验](schemars-integer-research.md)，建议 PID 等 u32 字段使用明确上下界的 JSON integer，完整 u64 序号、修订号和计数使用十进制字符串。领域类型集中表达运行时解析和 Schema；边界样本覆盖最大值、越界值、空值及规范拼写。

字符串的数字范围需要字符串约束表达。仅有数字字符 pattern 和长度上限仍可能接受越界值，需与运行时完整范围检查一致。

JSON-RPC 的 ID 关联不取代日志的 `stream_id`、`seq` 和 `gap`。它们继续表达日志流和缺口。现有调用期限、断连后的结果确认及系统报告的会话匹配也属于需要保留并测试的领域行为。

## Rust 工具候选

以下为官方文档或源码静态调查结果，尚未完成 Venera 全模型集成实验：

| 工具 | 已核对的能力及适配点 |
|---|---|
| [jsonrpsee 0.26.1](https://docs.rs/jsonrpsee/0.26.1/jsonrpsee/) | 提供 RPC 核心、宏和客户端传输抽象；与当前串行循环的接入需要实测 |
| [openrpsee 0.3.2](https://docs.rs/openrpsee/0.3.2/openrpsee/) | 从 trait 方法与类型生成 OpenRPC；[文档模型源码](https://docs.rs/openrpsee/0.3.2/src/openrpsee/openrpc.rs.html) 的 result 必有，与通知专用方法需要额外适配 |
| [openrpc-types 0.5.0](https://docs.rs/openrpc-types/0.5.0/openrpc_types/) | OpenRPC 文档数据模型；方法及错误码唯一性仍需验证 |
| [OpenRPC typings](https://open-rpc.github.io/typings/) | 提供类型生成工具，包括 Rust 输出入口；具体模型覆盖需要实测 |

[OpenRPC Method Object](https://spec.open-rpc.org/#method-object) 规定省略 result 的方法作为 notification 使用，因此通知必须纳入生成器覆盖实验。

## 下一步评审与验证

建议确认 code-first 契约源、双端接收接口文档和标准 batch 行为后，以现有状态、筛选、日志与系统信息类型进行小型生成实验。验收包括全部引用可解析、OpenRPC 元 Schema 校验通过、Schema 与实际编解码共享样本、方法注册与发现一致，以及双向发现和版本识别行为。

工具依赖在覆盖实验后确定。研究阶段已经核对规范与固定版本源码；真实 RPC 迁移、生成器集成和 Windows 部署仍属于后续实现。
