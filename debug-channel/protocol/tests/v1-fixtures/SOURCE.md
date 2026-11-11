# Validation fixtures

- `openrpc-1.14.9.json`: OpenRPC meta-schema release 1.14.9, https://github.com/open-rpc/meta-schema/releases/tag/1.14.9, asset `open-rpc-meta-schema.json`.
- `json-schema-tools-1.8.3.json`: JSON Schema Tools meta-schema 1.8.3, https://github.com/json-schema-tools/meta-schema/blob/1.8.3/schema.json.
- Contract target: OpenRPC specification [v1.4.1](https://github.com/open-rpc/spec/tree/v1.4.1), with Draft-07 Schema Objects.

The vendored OpenRPC fixture is the official 1.14.9 release asset. Its version enumeration includes specification versions through `1.3.2`. The validator builds an in-memory `1.4.1` compatibility profile by adding `1.4.1` to that enumeration, using the official document structure. Validation also performs method-name, parameter-order, reference-resolution and Draft-07 checks.
