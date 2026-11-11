# Schemars 与 JSON 整数契约

## 结论

Schemars 可以从 Rust 类型生成 JSON Schema，并与 `serde_with::DisplayFromStr` 集成，将 Rust 整数映射为 JSON 字符串。实际数值解析仍由 Serde 和适配器负责；自定义反序列化函数的行为需要对应的 Schema 定义与共享测试样本。

当前协议的浮点回退在实验中产生了精度损失。单独启用 `serde_json/arbitrary_precision` 可以改善 `Value` 的数字表示，但后续调用 `as_f64()` 仍会舍入。

## 实验环境与复现

本次实验使用 Schemars 1.2.3、Serde 1.0.229、serde_json 1.0.154、serde_with 3.24.0。独立项目及锁文件位于 `/tmp/persist/opencode/schemars-integer-experiment/`。

从仓库目录执行：

```bash
bash /tmp/persist/opencode/schemars-integer-experiment/reproduce.sh
```

原始输出为该目录的 `default.log`、`arbitrary.log`。主会话独立复跑返回 0，记录为 `primary-recheck.log`。项目的 `src/main.rs` 包含完整实验输入与实现。

## 数字输入与精度

“浮点回退”复现当前协议的 `as_u64()`，继而 `as_f64()`、范围及小数部分检查的路径。

| JSON 输入 | 普通 `u64` | 默认浮点回退 | 开启 `arbitrary_precision` 的浮点回退 |
|---|---|---|---|
| `1` | `1` | `1` | `1` |
| `1.0` | 类型错误 | `1` | `1` |
| `1e0` | 类型错误 | `1` | `1` |
| `9007199254740993` | 原值 | 原值 | 原值 |
| `9007199254740993.0` | 类型错误 | `9007199254740994` | `9007199254740992` |
| `18446744073709551615` | 原值 | 原值 | 原值 |
| `1.0000000000000001` | 类型错误 | `1` | `1` |

普通 `u64` 在两种配置下的结果一致。`arbitrary_precision` 保留高精度小数的 `Value` 表示，转换成 `f64` 后仍有精度损失；它也没有改变普通 `u64` 对 `1.0` 的接受行为。数字重新输出时可能改变拼写，例如 `1e0` 输出为 `1e+0`。

## 默认生成的 Schema

本次默认生成结果中，`u32` 和 `u64` 分别具有 `format: uint32`、`format: uint64`，类型均为 `integer`，并具有 `minimum: 0`。生成结果没有 `maximum`。因此完整的取值上界需要额外表达与验证，不能仅依赖格式标记。

`Option<u64>` 允许 `integer` 或 `null`，默认生成的对象必填列表包含普通整数字段，省略可选字段。

给 `u64` 字段添加 `#[serde(deserialize_with = "custom")]` 后，`JsonSchema` 仍能派生，但生成结果仍描述字段类型 `u64`。它没有推导函数的浮点回退逻辑。实验还验证了 `#[schemars(with = "u64")]` 和 `schema_with` 回调可用；前者在本例中生成相同字段约束，后者提供自定义 Schema 的入口。

## 十进制字符串适配

`serde_with` 3.24.0 提供可用的 `schemars_1` 集成。启用该特性及 Schemars 派生支持后，可以使用：

```rust
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, DisplayFromStr};

#[serde_as]
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Counters {
    #[serde_as(as = "DisplayFromStr")]
    big: u64,
    #[serde_as(as = "Option<DisplayFromStr>")]
    #[serde(default)]
    optional: Option<u64>,
}
```

实验中两个字段的最大 `u64` 十进制字符串均完整往返。数值形式输入被适配器拒绝；可选字段缺省或为 `null` 时得到 `None`。

生成的 Schema 分别为字符串及可空字符串，没有 `pattern`。因此，字符串数字的语法和范围需要额外 Schema 约束与样本验证，才能与 Rust 解析行为精确对应。

## 对 Venera 协议的应用

适合采用 Rust 消息类型生成 Schema，并以同一批字面样本交叉验证 Schema 与编解码器。对需要完整 64 位精度的字段，十进制字符串配合现成适配器是可验证的选择。已有数字字段改为字符串会改变线格式，需要与协议版本及兼容策略一起确定。

本次成果为独立实验与选型依据；协议字段表示保持现状。

## 官方资料

- [Schemars 官方文档](https://graham.cool/schemars/)
- [serde_with 3.24.0：DisplayFromStr](https://docs.rs/serde_with/3.24.0/serde_with/struct.DisplayFromStr.html)

### 固定版本源码核对

- Schemars 的 [`primitives.rs`](https://github.com/GREsau/schemars/blob/v1.2.3/schemars/src/json_schema_impls/primitives.rs#L92-L115) 通过宏为 `u32/u64` 生成类型、格式和下界，与实验输出一致。
- [JSON Schema 2020-12 §6.1.1](https://json-schema.org/draft/2020-12/json-schema-validation#section-6.1.1) 将 integer 定义为小数部分为零的数，因此包括 `1.0` 和 `1e0`。[§7.2](https://json-schema.org/draft/2020-12/json-schema-validation#section-7.2) 描述 format 的注解与断言机制。数值 Schema 描述解析后的值；原始数字 token 的拼写需要在解析层处理。
- Schemars 的[属性文档](https://graham.cool/schemars/deriving/attributes/) 提供 `with`、`schema_with`、`range`、`regex` 和 `transform`。固定版本的 [range 生成代码](https://github.com/GREsau/schemars/blob/v1.2.3/schemars_derive/src/attr/validation.rs#L114-L132) 与[值插入实现](https://github.com/GREsau/schemars/blob/v1.2.3/schemars/src/_private/mod.rs#L338-L346) 支持以 JSON 数值表达范围端点；这是源码核对结果，本次实验尚未单独验证 range 注解。
- [`serde_with/schemars_1.rs`](https://github.com/jonasbb/serde_with/blob/v3.24.0/serde_with/src/schemars_1.rs#L407-L409) 将 `DisplayFromStr` 的 Schema 转发给 `String`；其[解码实现](https://github.com/jonasbb/serde_with/blob/v3.24.0/serde_with/src/de/impls.rs#L1026-L1056) 读取字符串并调用 `FromStr`。接受文法由该转换决定，若定义规范十进制形式，需让 Schema 和实际解析共同表达该规则。
- serde_json 的 [`Number`](https://github.com/serde-rs/json/blob/v1.0.154/src/number.rs#L149-L170) 分别实现整数解析和浮点转换；[`as_str()` 文档](https://github.com/serde-rs/json/blob/v1.0.154/src/number.rs#L282-L309) 说明了指数表示的规范化。
- Schemars 的 [`SchemaSettings`](https://docs.rs/schemars/1.2.3/schemars/generate/struct.SchemaSettings.html) 支持分别生成 serialize 与 deserialize 契约；[转换类型处理源码](https://github.com/GREsau/schemars/blob/v1.2.3/schemars_derive/src/schema_exprs.rs#L56-L106) 对应 Serde 的 `from/try_from/into`。转换函数内部的校验逻辑仍需要对应 Schema 定义。
