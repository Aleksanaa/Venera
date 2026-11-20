# Windows Debug Background Service

`venera-debug-guest.exe` collects debug output from logon sessions, system debug events, and Venera engine records into Windows history. Panel displays live records locally, and Linux clients archive history over VirtIO sockets. Windows listens on port 49152 through the viosock Winsock provider; Linux clients connect using the VM's guest CID. The service is named `VeneraDebugGuest` and starts automatically as LocalSystem. Each logon session runs the same program in `session-helper` mode, including disconnected RDP sessions that remain logged on.

## Windows History

The service saves captured records under `%ProgramData%\Venera\DebugHistory`, including while the host is disconnected or forwarding is paused. Display selections operate on the retained records.

Default retention keeps startup records from the latest three Windows boots and other runtime records from the most recent five hours. Startup covers the first two minutes of each boot. Capture files and the journal share a physical storage budget of 1 GiB; capacity cleanup can expire older records earlier. A service restart continues the same Windows boot and journal history.

Each retained event has a journal cursor, Windows boot identity, recording time, and startup classification. Consumers resume using their cursor. When a requested interval has expired, history reports its positions as a `retention_expired` gap and continues with the available records.

## Service Installation

Run `venera-setup.exe` and choose the components for this guest. Graphics is required. Panel, Debug collector, and VirtIO debug transport are selectable; the collector uses Panel and the transport. The transport package includes the official driver, both Windows provider libraries, and provider service. Configure the VM's [VirtIO socket device and CID](virtio-socket-host.md#qemu-and-libvirt-guest-address) for host access.

For unattended installation with collection:

```powershell
.\venera-setup.exe /S /components=graphics,panel,collector,transport
```

Run setup again to modify the selection or repair the installed components. Examples from elevated PowerShell:

```powershell
.\venera-setup.exe /modify /S /components=graphics,panel,collector,transport
.\venera-setup.exe /repair /S
.\venera-setup.exe /uninstall /S
```

The installer remembers the selected components for maintenance and upgrades. The control panel installs to `%ProgramFiles%\Venera`; it locates the collector through the service configuration. Panel Settings offers System, English, and Simplified Chinese language preferences. A selection applies immediately and persists across launches.

For manual deployment, place `venera-debug-guest.exe` in a stable directory, switch to that directory, and register and start it from an elevated PowerShell:

```powershell
.\venera-debug-guest.exe install-service
Start-Service VeneraDebugGuest
```

The service startup command is `"full-path\venera-debug-guest.exe" service`. Launching the program directly also enters the service dispatcher. To uninstall:

```powershell
.\venera-debug-guest.exe uninstall-service
```

## Panel and Command Line

Successful control commands return one line of UTF-8 JSON. The panel uses the following commands to query and control the service and parses their results. For command-line use, append the arguments in the table to `venera-debug-guest.exe`.

| Command | Returned content |
|---|---|
| `status --json` | Shared `Status` object: `forwarding_enabled`, `revision`, `effective_filter`, `expires_at`, `health`, `loss`. |
| `forwarding on` / `forwarding off` | `{"ok":true,"status":{...}}` with the applied forwarding state. Run elevated. |
| `filter-set --json FILE` | `{"ok":true,"status":{...}}` with the applied display filter and revision. Run elevated. |
| `filter-reset` | `{"ok":true,"status":{...}}` with the default display filter and revision. Run elevated. |
| `logs --follow` | Continuous UTF-8 JSONL containing selected `log` records and `gap` records. |

`logs --follow` begins at the current history position and displays new records. During a connection interruption, the running command retains its consumed cursor and resumes from that position. Starting a new command begins at the current tail again. Progress advances after output is flushed, including positions consumed under a display filter.

Forwarding controls delivery to the host. With forwarding paused, Windows history and local Panel display continue. When forwarding resumes, connected host collectors catch up from their saved progress. The forwarding choice persists across service restarts and defaults to enabled on first use.

The `filter-set` file uses UTF-8 JSON:

```json
{
  "filter": {
    "components": ["dxvk", "venus"],
    "levels": [],
    "targets": [
      {"kind":"exe_path","path":"C:\\Games\\demo.exe"},
      {"kind":"process_instance","pid":4120,"start_time_filetime":"134124000000000000","include_descendants":true}
    ]
  },
  "duration_ms": 60000
}
```

The file can also contain a shared `Filter` object directly. Empty arrays select all values in that dimension. Component and level selections combine with any matching process target. An executable-path target selects that program; a process-instance target uses both PID and creation FILETIME, optionally including descendants. A successful change increments the service revision. Expiration restores the default filter. The service starts with the default filter.

These service filters select the Windows live display used by `logs --follow` and Panel. Panel's log-page controls further narrow that display. The Web workbench independently queries the complete host archive using its own component, level, program, text and time selections.

Status includes the applied filter, decimal-string revision, expiration, collection health and loss counters. The CLI reports an unsuccessful request as a command failure with its diagnostic message.

## Connections and History Access

Local clients connect through `\\.\pipe\VeneraDebugGuest.v1`. Both the named pipe and VirtIO socket use JSON-RPC 2.0 over UTF-8 JSONL, with `endpoint.info` and `rpc.discover` negotiation. Each client connection maintains its own request correlation and history progress.

`history.range` gives the current journal identity, oldest position and high water. `history.read` returns a page of retained events and expired intervals, and `history.ack` acknowledges consumed progress. Host archival and interactive controls use independent connections. Local history access remains available when host forwarding is paused. See the [protocol reference](venera-protocol-v1.md) for request and response formats.

## Sources and Metadata

Recognized components are `venera_d3d11`, `venera_d3d12`, `mesa`, `venus`, `dxvk`, `vkd3d`, `vkd3d-shader`, `kernel`, and `service`. Venus uses the `MESA-VIRTIO` tag. Service/helper logs are identified by actual executable paths in the installation directory. For structured Venera driver records, `structured.event_id` identifies kernel events (1–4), D3D11 messages (5), and D3D12 messages (6).

On installations where Venera owns GlobalLogger, the service imports legacy kernel `DbgPrint` messages from its boot trace. Setup preserves an existing externally managed GlobalLogger configuration; on those systems, early `DbgPrint` coverage depends on the external logger settings. Venera's structured kernel events use a separate boot trace.

DXVK/vkd3d `VDBG1` records are reassembled by producer, thread, sequence, and fragment number from the native adapter. Original bytes are concatenated and decoded as strict UTF-8. Ordinary DBWIN content is decoded as UTF-8 first; other bytes are converted using the current Windows ANSI code page. Complete text is retained in `text`, and the original level in `raw_level`. Sources without a level use `unknown`.

The canonical record preserves `observed_at`, the collection timestamp, separately from `source_time`, the timestamp supplied by the source. ETW source timestamps use UTC with millisecond precision. Panel displays the original observation time in local time. Optional unknown metadata uses the protocol's null values.

Log `seq` is a decimal-string sequence within `stream_id`. The durable history cursor is a separate pair of `journal_id` and decimal-string `position`. Retained events in follow output also include `boot_id`, decimal-string `recorded_at_ms`, and `startup`, allowing consumers to distinguish observation, recording and history order. Expired intervals carry `first_position` and `last_position`.

The service tracks process creation and exit events and supplements them with process snapshots. A record carries the available process identity and an optional `ancestors` chain, nearest parent first. Each instance uses PID and creation FILETIME, plus the executable path when available. The saved identities support historical descendant matching after a parent exits or its PID is reused.

Native reassembly losses, session-helper capture losses and incremental ETW event losses are retained as `gap` events. If a sealed trace reports lost events, the journal stores a `gap` record with its own cursor and a reason beginning with `capture_loss`. In contrast, `history.read` lists expired journal positions in the page's `missing` intervals with reason `retention_expired`.

## Build

Run from the repository root:

```sh
nix develop -c cargo test --locked --manifest-path debug-channel/Cargo.toml -p venera-debug-guest
nix develop -c cargo build --locked --release --manifest-path debug-channel/Cargo.toml -p venera-debug-guest --target x86_64-pc-windows-gnu --target-dir build/debug-channel
nix develop -c cargo test --locked --release --no-run --manifest-path debug-channel/Cargo.toml -p venera-debug-guest --target x86_64-pc-windows-gnu --target-dir build/debug-channel
```

The service executable is `build/debug-channel/x86_64-pc-windows-gnu/release/venera-debug-guest.exe`. The test build prints the Windows test executable path; that executable includes the native named-pipe RPC and connection-lifecycle tests.

Installer builds run `scripts/fetch-viosock.sh` to prepare virtio-win 0.1.302 under `build/viosock/package`.
For an existing complete package, set `VENERA_VIOSOCK_DIR` to its directory before building. The package contains
`viosock.inf`, `viosock.cat`, `viosock.sys`, `viosocklib_x64.dll`, `viosocklib_x86.dll`, and
`viosockwspsvc.exe`, together with its license materials. See the [VirtIO socket distribution](viosock-distribution.md) for package contents and licensing.
