# Venera 协议 1.0.0

Rust 类型与方法声明生成双端 OpenRPC 1.4.1 接收接口文档。消息采用 JSON-RPC 2.0，VirtIO Serial 传输采用 UTF-8 JSONL。

## 生成与验证

```bash
cargo run --manifest-path debug-channel/Cargo.toml -p venera-debug-protocol --example generate-v1 --features schema
cargo run --manifest-path debug-channel/Cargo.toml -p venera-debug-protocol --example generate-v1 --features schema -- --check
```

## 接收接口

| 接收端 | 方法 | 消息 |
|---|---|---|
| guest | `endpoint.info` | request |
| guest | `status.get` | request |
| guest | `filter.set` | request |
| guest | `filter.reset` | request |
| guest | `system.info.get` | request |
| guest | `rpc.discover` | request |
| host | `endpoint.info` | request |
| host | `rpc.discover` | request |
| host | `status.changed` | notification |
| host | `log` | notification |
| host | `gap` | notification |

## 端点协商

`endpoint.info` 返回端点角色（host 或 guest）、软件版本及协议版本。当前端点生成的协议版本为 `1.0.0`。协议版本采用稳定三段 SemVer；同一连接的端点信息与发现文档采用相同版本。对端完成两项信息交换后，按发现文档中的实际方法选择调用。每次连接重新交换信息。

## 消息与传输

Venera 发起的请求使用字符串 ID。接收端支持字符串、数值及 null ID，响应回显原 ID。具名参数采用对象；省略参数时按空对象解码。普通数据对象兼容额外字段，过滤目标按 kind 对应的字段组合解码。响应采用 result 或 error 形式。

省略 ID 的消息作为通知执行，业务调用也支持此形式。非空数组承载批消息；调用完成后聚合为一个响应数组，按输入顺序排列。通知完成副作用处理。

UTF-8 JSONL 每行承载一个对象或批数组，保留接收的原始字节；空白行跳过。单行容量为 1 MiB，超长帧在下一行恢复解析。

## 数据表示

序号、修订、计数、持续毫秒和 FILETIME 使用规范十进制 u64 字符串，范围为 `0` 至 `18446744073709551615`。PID、线程 ID 和会话 ID 使用范围为 `0` 至 `4294967295` 的 JSON 整数。

Serde 的 u32 解码采用整数词法；Draft-07 的 integer 采用数学整数语义，因此 `1.0` 和 `1e0` 在 Schema 中有效，Serde 解码返回类型错误。共享样本分别记录两侧结果。

参数 Schema 按反序列化契约生成，结果 Schema 按序列化契约生成；组件名的 Input/Output 前缀区分字段必填与可空规则。Schema 方言为 Draft-07，引用指向 `/components/schemas`。可选字段在输入中支持缺省及 null，输出按类型的 Serde 属性生成。

## 验证入口

```bash
cargo test --manifest-path debug-channel/Cargo.toml -p venera-debug-protocol --features schema
python3 debug-channel/protocol/tests/v1_schema.py
```

Python 验证使用 jsonschema，覆盖固定版本 OpenRPC 元 Schema、组件引用与共享数据样本。元 Schema 来源记录见测试 fixture 的 SOURCE.md。运行通道与查看工具的接入说明见 [调试通道](virtio-serial-debug.md)。
