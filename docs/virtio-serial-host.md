# Venera Host Debug Records

The host background process continuously receives records from the QEMU debug port, saving guest logs, effective filters, connection changes, and channel diagnostics. History is stored in SQLite in reception order, with the oldest records evicted when capacity reaches the budget. Log levels retain both the original and normalized values supplied by the collector.

## Build and Run

```sh
cd debug-channel
cargo build --release -p venera-debug-host
```

QEMU provides a Unix socket listener for the dedicated virtserialport. The background process connects to that path and continuously reconnects:

```sh
venera-debug-host receive \
  --socket /run/user/1000/venera-debug.sock \
  --control /run/user/1000/venera-control.sock \
  --database /home/user/.local/share/venera/history.db \
  --max-bytes 1073741824
```

Create the example history directory first, and replace the paths with actual paths for the current user. `receive` runs continuously and can be managed by `systemd --user`. On each connection, the background process sends an endpoint introduction and a status query.

`--max-bytes` is the total capacity budget for the history store and SQLite workspace, with a minimum of 65536 bytes. Transaction workspace is reserved, and history uses the remaining capacity. The oldest history records are evicted in batches, and database pages are reclaimed on commit. Storage errors are reported through background runtime diagnostics.

## User Service Deployment

Run from the repository root and choose an accessible QEMU socket path. These commands install the build artifact in the user's program directory and render the startup command in the service template:

```sh
nix develop --command cargo build --manifest-path debug-channel/Cargo.toml --release -p venera-debug-host
install -Dm755 debug-channel/target/release/venera-debug-host "$HOME/.local/bin/venera-debug-host"
mkdir -p "$HOME/.local/share/venera" "$XDG_RUNTIME_DIR/venera"
python3 scripts/venera-debug-host-service \
  --binary "$HOME/.local/bin/venera-debug-host" \
  --socket "$XDG_RUNTIME_DIR/venera/debug.sock" \
  --control "$XDG_RUNTIME_DIR/venera/control.sock" \
  --database "$HOME/.local/share/venera/history.db" \
  --max-bytes 1073741824 \
  --output "$HOME/.config/systemd/user/venera-debug-host.service"
systemd-analyze --user verify "$HOME/.config/systemd/user/venera-debug-host.service"
systemctl --user daemon-reload
systemctl --user enable --now venera-debug-host.service
systemctl --user status venera-debug-host.service
journalctl --user -u venera-debug-host.service -f
```

The template is `debug-channel/host/systemd/venera-debug-host.service.in`. The script accepts installed absolute paths and converts them to systemd argument syntax. The user service manager restarts the service after runtime failures. The service manager creates the `$XDG_RUNTIME_DIR/venera` runtime directory. If QEMU uses a persistent directory, replace `--socket` and `--control` with paths in that directory.

```sh
systemctl --user stop venera-debug-host.service
systemctl --user start venera-debug-host.service
"$HOME/.local/bin/venera-debug-host" status --control "$XDG_RUNTIME_DIR/venera/control.sock"
```

Stopping the service causes the service manager to send a termination signal. Committed history remains available on the next start. The user service follows the user service manager's lifecycle; for deployments that need continuous operation after boot, an administrator can configure lingering for that user.

## QEMU and libvirt Ports

Example QEMU arguments, using the bus name of an existing virtio-serial controller in the VM:

```sh
-chardev socket,id=venera-debug,path=/run/user/1000/venera/debug.sock,server=on,wait=off \
-device virtserialport,bus=virtio-serial0.0,chardev=venera-debug,name=org.venera.debug.0
```

For a libvirt VM, append this channel to the existing `<devices>`. Its path should match the host `--socket` and be accessible to both the QEMU runtime user and the host background user:

```xml
<channel type='unix'>
  <source mode='bind' path='/run/user/1000/venera/debug.sock'/>
  <target type='virtio' name='org.venera.debug.0'/>
</channel>
```

libvirt's `bind` mode creates a listening socket; after startup QEMU waits for the background process to connect. The corresponding command-line form is `server=on,wait=off`. Existing SPICE channels and other devices remain alongside the new channel, and libvirt allocates an available virtio-serial port. System libvirt commonly uses domain-specific directories under `/var/lib/libvirt/qemu/channel/target/`. Configure the actual socket directory and permissions for the current domain's runtime user. After saving the domain configuration, enable the port during the planned VM restart.

## Queries and Filters

```sh
venera-debug-host status --control /run/user/1000/venera-control.sock
venera-debug-host filter-set --control /run/user/1000/venera-control.sock \
  --filter '{"components":["dxvk"],"levels":["warning","error"],"targets":[]}' \
  --duration-ms 60000
venera-debug-host filter-reset --control /run/user/1000/venera-control.sock
```

Commands submit requests to the continuously running background process, which forwards them to the guest and returns the corresponding response. The effective filter, revision, expiration time, and forwarding state in the response come from the guest. `status` queries the current effective state.

Filters select already-produced records. Component, level, and target dimensions are intersected, while values within each dimension are unioned. An empty array matches all values in that dimension. All three arrays are empty in the default filter. Targets support process instances and full executable paths.

## Save and Export

```sh
venera-debug-host export --database /home/user/.local/share/venera/history.db \
  > session.jsonl
```

Select an export range by host reception time:

```sh
venera-debug-host export --database /home/user/.local/share/venera/history.db \
  --from 2026-11-10T08:00:00Z --until 2026-11-10T09:00:00Z \
  > session-hour.jsonl
```

`--from` includes the start; `--until` stops before the end. Either can be used alone. Times use RFC3339, supporting fractional seconds and timezone offsets, and are compared by their corresponding UTC instants. The range applies to log, status, connection, and gap records using host `received_at`. When both arguments are provided, the start should precede the end. By default, all current history is exported.

Export generates JSON line by line. Each line contains host reception time in UTC as `received_at` and the original message object as `message`, preserving additional message fields. Export determines the end position at startup and reads the history still retained at that point in reception order. The background process continues receiving and evicting during export; history evicted before it is read is skipped. Storage IDs increase persistently, and the export cutoff identifies the history batch at startup. To preserve a complete current history batch, stop the service before exporting. User-saved files are maintained independently of the background history capacity.

## Shared Rust Interface

The guest shares the contract through the `venera-debug-protocol` path dependency. Main types are `Envelope`, `Message`, `LogRecord`, `ProcessIdentity`, `Filter`, `Target`, `Status`, `RequestResult`, and `RequestError`.

`Envelope::new(Message)` creates a version 1 message; `encode` produces LF-terminated UTF-8 JSONL, and `decode` parses one message. `Framer::new(max_bytes)`, `push`, and `reset` support incremental reads; `push_raw` returns both the typed message and the complete JSON object. `DecodeError` distinguishes encoding, JSON, version, unknown-type, and line-length errors.

`matches_filter(&filter, &record, &ancestors)` performs pure-function matching. The caller supplies `ancestors` as ancestor process instances confirmed within the current rule's lifetime; the guest maintains rule management and instance tracking. Path inputs use full paths queried by the guest. Windows builds use system ordinal case-insensitive comparison; Linux builds use Unicode lowercase comparison.

The machine-readable contract is `debug-channel/protocol/schema.json`; literal samples are in `debug-channel/protocol/tests/vectors.jsonl`.

## Validation

```sh
cd debug-channel
cargo fmt --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
python3 protocol/tests/schema.py
python3 host/tests/deployment.py
```

The schema validation script uses Python `jsonschema`. Rust tests cover byte splitting, recovery from malformed lines, filter matching, and real Unix sockets, CLI controls, reconnects, restarts, SQLite eviction, and export.

Run host integration checks from the repository root:

```sh
nix develop --command cargo test --manifest-path debug-channel/Cargo.toml -p venera-debug-host
python3 debug-channel/host/tests/deployment.py
```
