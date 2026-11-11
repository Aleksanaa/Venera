//! Draft-07 generation for each receiving endpoint.
use super::{Discovery, Receiver};
use schemars::{generate::SchemaSettings, SchemaGenerator};
use serde_json::{json, Map, Value};

pub(super) struct Method {
    pub name: &'static str,
    pub params: Value,
    pub result: Option<Value>,
}

fn generator(output: bool) -> SchemaGenerator {
    let mut settings = SchemaSettings::draft07();
    settings.definitions_path = "/components/schemas".into();
    if output {
        settings.for_serialize()
    } else {
        settings.for_deserialize()
    }
    .into_generator()
}

fn qualify(value: &mut Value, prefix: &str) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get_mut("$ref") {
                if let Some(name) = reference.strip_prefix("#/components/schemas/") {
                    *reference = format!("#/components/schemas/{prefix}{name}");
                }
            }
            for child in map.values_mut() {
                qualify(child, prefix);
            }
        }
        Value::Array(array) => {
            for child in array {
                qualify(child, prefix);
            }
        }
        _ => (),
    }
}

fn collect(generator: &mut SchemaGenerator, prefix: &str, schemas: &mut Map<String, Value>) {
    for (name, mut schema) in generator.take_definitions(true) {
        qualify(&mut schema, prefix);
        schemas.insert(format!("{prefix}{name}"), schema);
    }
}

pub fn document(receiver: Receiver) -> Discovery {
    let mut input = generator(false);
    let mut output = generator(true);
    let mut declarations = super::declarations(receiver, &mut input, &mut output);
    let mut schemas = Map::new();
    // Contract-specific names preserve nullable/required differences in shared DTOs.
    collect(&mut input, "Input", &mut schemas);
    collect(&mut output, "Output", &mut schemas);
    let mut methods = Vec::new();
    for method in &mut declarations {
        qualify(&mut method.params, "Input");
        let reference = method.params["$ref"]
            .as_str()
            .expect("parameter object reference");
        let name = reference.strip_prefix("#/components/schemas/").unwrap();
        let object = &schemas[name];
        let required = object["required"].as_array();
        let mut params: Vec<_> = object["properties"].as_object().into_iter().flatten().map(|(name, schema)| {
            json!({"name": name, "required": required.is_some_and(|list| list.contains(&json!(name))), "schema": schema})
        }).collect();
        params.sort_by_key(|param| param["required"] != true);
        let mut entry = json!({"name": method.name, "paramStructure": "by-name", "params": params});
        if let Some(result) = &mut method.result {
            qualify(result, "Output");
            entry["result"] = json!({"name": "result", "schema": result});
        }
        methods.push(entry);
    }
    serde_json::from_value(json!({
        "openrpc": "1.4.1",
        "info": {"title": match receiver { Receiver::Guest => "Venera guest", Receiver::Host => "Venera host" }, "version": super::PROTOCOL_VERSION},
        "methods": methods,
        "components": {"schemas": schemas},
    })).expect("generated discovery document")
}

pub fn reference() -> String {
    let mut text = String::from("# Venera 协议 1.0.0\n\nRust 类型与方法声明生成双端 OpenRPC 1.4.1 接收接口文档。消息采用 JSON-RPC 2.0，VirtIO Serial 传输采用 UTF-8 JSONL。\n\n## 生成与验证\n\n```bash\ncargo run --manifest-path debug-channel/Cargo.toml -p venera-debug-protocol --example generate-v1 --features schema\ncargo run --manifest-path debug-channel/Cargo.toml -p venera-debug-protocol --example generate-v1 --features schema -- --check\n```\n\n## 接收接口\n\n| 接收端 | 方法 | 消息 |\n|---|---|---|\n");
    for (receiver, role) in [(Receiver::Guest, "guest"), (Receiver::Host, "host")] {
        for method in document(receiver).methods {
            text.push_str(&format!(
                "| {role} | `{}` | {} |\n",
                method.name,
                if method.result.is_some() {
                    "request"
                } else {
                    "notification"
                }
            ));
        }
    }
    text.push_str("\n## 端点协商\n\n`endpoint.info` 返回端点角色（host 或 guest）、软件版本及协议版本。当前端点生成的协议版本为 `1.0.0`。协议版本采用稳定三段 SemVer；同一连接的端点信息与发现文档采用相同版本。对端完成两项信息交换后，按发现文档中的实际方法选择调用。每次连接重新交换信息。\n\n## 消息与传输\n\nVenera 发起的请求使用字符串 ID。接收端支持字符串、数值及 null ID，响应回显原 ID。具名参数采用对象；省略参数时按空对象解码。普通数据对象兼容额外字段，过滤目标按 kind 对应的字段组合解码。响应采用 result 或 error 形式。\n\n省略 ID 的消息作为通知执行，业务调用也支持此形式。非空数组承载批消息；调用完成后聚合为一个响应数组，按输入顺序排列。通知完成副作用处理。\n\nUTF-8 JSONL 每行承载一个对象或批数组，保留接收的原始字节；空白行跳过。单行容量为 1 MiB，超长帧在下一行恢复解析。\n\n## 数据表示\n\n序号、修订、计数、持续毫秒和 FILETIME 使用规范十进制 u64 字符串，范围为 `0` 至 `18446744073709551615`。PID、线程 ID 和会话 ID 使用范围为 `0` 至 `4294967295` 的 JSON 整数。\n\nSerde 的 u32 解码采用整数词法；Draft-07 的 integer 采用数学整数语义，因此 `1.0` 和 `1e0` 在 Schema 中有效，Serde 解码返回类型错误。共享样本分别记录两侧结果。\n\n参数 Schema 按反序列化契约生成，结果 Schema 按序列化契约生成；组件名的 Input/Output 前缀区分字段必填与可空规则。Schema 方言为 Draft-07，引用指向 `/components/schemas`。可选字段在输入中支持缺省及 null，输出按类型的 Serde 属性生成。\n\n## 验证入口\n\n```bash\ncargo test --manifest-path debug-channel/Cargo.toml -p venera-debug-protocol --features schema\npython3 debug-channel/protocol/tests/v1_schema.py\n```\n\nPython 验证使用 jsonschema，覆盖固定版本 OpenRPC 元 Schema、组件引用与共享数据样本。元 Schema 来源记录见测试 fixture 的 SOURCE.md。运行通道与查看工具的接入说明见 [调试通道](virtio-serial-debug.md)。\n");
    text
}
