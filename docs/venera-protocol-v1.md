# Venera Protocol 1.0.0

Rust types and method declarations generate OpenRPC 1.4.1 receiving interface documents for both endpoints. Messages use JSON-RPC 2.0, with UTF-8 JSONL transport over VirtIO socket.

## Generation and Validation

```bash
cargo run --manifest-path debug-channel/Cargo.toml -p venera-debug-protocol --example generate-v1 --features schema
cargo run --manifest-path debug-channel/Cargo.toml -p venera-debug-protocol --example generate-v1 --features schema -- --check
```

## Receiving Interfaces

| Receiver | Method | Message |
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

## Endpoint Negotiation

`endpoint.info` returns the endpoint role (host or guest), software version, and protocol version. Endpoints currently generate protocol version `1.0.0`. Protocol versions use stable three-part SemVer; endpoint information and discovery documents on a connection use the same version. After exchanging both pieces of information, peers select calls from the actual methods in the discovery document. Information is exchanged anew on each connection.

## Messages and Transport

Requests initiated by Venera use string IDs. Receivers support string, numeric, and null IDs and echo the original ID in responses. Named parameters use objects; omitted parameters decode as an empty object. Ordinary data objects accept additional fields, and filter targets decode according to the field combination associated with their kind. Responses use the result or error form.

Messages with an omitted ID execute as notifications; business calls also support this form. Nonempty arrays carry batches. After calls complete, responses are aggregated into one response array in input order. Notifications complete their side-effect processing.

Each UTF-8 JSONL line carries one object or batch array, preserving the original received bytes; blank lines are skipped. The line limit is 1 MiB, and parsing resumes on the next line after an oversized frame.

## Data Representation

Sequence numbers, revisions, counts, durations in milliseconds, and FILETIME values use canonical decimal u64 strings ranging from `0` to `18446744073709551615`. PIDs, thread IDs, and session IDs use JSON integers ranging from `0` to `4294967295`.

Serde u32 decoding uses integer lexical syntax; Draft-07 integer uses mathematical integer semantics. Thus `1.0` and `1e0` are valid under the Schema and produce type errors in Serde decoding. Shared samples record each side's result separately.

Parameter Schemas follow the deserialization contract, and result Schemas follow the serialization contract. Input/Output prefixes on component names distinguish required-field and nullability rules. The Schema dialect is Draft-07, with references pointing to `/components/schemas`. Optional input fields support omission and null; output follows the type's Serde attributes.

## System Report Messages

Report sections carry `title` and optional `title_message`; rows carry `key`, `value`, `warning` and optional `key_message`, `value_message`, `warning_message`. Each message has a stable semantic `id` such as `report.driver`, `report.notInstalled`, or `report.memory`, an `args` array of raw strings (defaulting to an empty array), and an optional `diagnostic` suffix. Templates substitute numbered placeholders such as `{0}` with the corresponding argument, then append the diagnostic verbatim. The rendered fields provide English fallback text. Consumers use the original field when the message ID is unfamiliar or a required argument is missing. Hardware names, paths, API names, and versions retain their collected spelling.

## Validation Commands

```bash
cargo test --manifest-path debug-channel/Cargo.toml -p venera-debug-protocol --features schema
python3 debug-channel/protocol/tests/v1_schema.py
```

Python validation uses jsonschema to cover the pinned-version OpenRPC meta-Schema, component references, and shared data samples. The test fixture's SOURCE.md records the meta-Schema source. See the [debug channel](virtio-socket-host.md) for integration with the running channel and viewers.
