# Windows Debug Background Service

`venera-debug-guest.exe` aggregates debug output from logon sessions, system debug events, and Venera engine records, sending shared JSONL messages through `org.venera.debug.0`. The service is named `VeneraDebugGuest` and starts automatically as LocalSystem. Each logon session runs the same program in `session-helper` mode, including disconnected RDP sessions that remain logged on.

## Service Installation

Current release artifacts are `venera-debug-0.2.2.147.fd9eadb-r10.iso` and the same-named `.exe` installer. The integration validation version is integration v13, including the optional official VirtIO Serial driver package. r10 completed full Windows installation, normal reboot, and production collection validation; resource, WIM, 14-component, CAT, and SYS content comparisons passed. Select the VirtIO serial driver option during interactive installation. Unattended installation explicitly selects a driver option:

```powershell
.\venera-setup.exe /S /with-vioserial
# Use the existing VirtIO Serial driver
.\venera-setup.exe /S /skip-vioserial
```

To install the bundled VirtIO Serial driver separately:

```powershell
.\venera-setup.exe /vioserial /S
```

`/vioserial` also supports interactive installation. The control panel installs to `%ProgramFiles%\Venera`; its Debug channel page shares the driver installation entry point. The debug service executable enters the Windows Driver Store with the display driver package, and the INF registers the service. The panel locates the executable through the service configuration.

For manual deployment, place `venera-debug-guest.exe` in a stable directory, switch to that directory, and register and start it from an elevated PowerShell:

```powershell
.\venera-debug-guest.exe install-service
Start-Service VeneraDebugGuest
```

The service startup command is `"FULL_PATH\venera-debug-guest.exe" service`. Launching the program directly also enters the service dispatcher. To uninstall:

```powershell
.\venera-debug-guest.exe uninstall-service
```

## Panel and Command Line

Each query or change returns one line of UTF-8 JSON. The panel uses the following commands to query and control the service and parses their results. For command-line use, append the arguments in the table to `venera-debug-guest.exe`.

| Command | Returned content |
|---|---|
| `status --json` | Shared `Status` object: `forwarding_enabled`, `revision`, `effective_filter`, `expires_at`, `health`, `loss`. |
| `forwarding on` / `forwarding off` | `{"ok":true,"status":{...}}`. Requires elevation. |
| `filter-set --json FILE` | Shared `request.result` envelope containing the configuration actually applied by the service and its revision. Requires elevation. |
| `filter-reset` | Shared `request.result` envelope. Requires elevation. |
| `logs --follow` | Continuous JSONL: a shared `status` envelope first, followed by filtered `log` and status notifications. |

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

The file can also contain a shared `Filter` object directly. An empty array in any dimension means all values; components come from the set of recognized sources. A successful change increments the service revision. Expiration restores the default and publishes status. The service starts with the default filter. Local forwarding defaults to enabled, with its switch persisted in `HKLM\SOFTWARE\Venera\DebugChannel\ForwardingEnabled`.

An example structure for `status --json`:

```json
{"forwarding_enabled":true,"revision":0,"effective_filter":{"components":[],"levels":[],"targets":[]},"expires_at":null,"health":{"driver":"installed","controller":{"state":"running","problem":0},"port":"present","host":"connected","dbwin":"collecting","etw":"collecting","sessions":[{"session_id":1,"connection_state":0,"helper":"running","error":null}],"session_capture":{"1":"collecting"}},"loss":{"queue_dropped":0,"parse_errors":0}}
```

Status fields come from actual probes. Collection or device failures populate the corresponding `error` field. Failed changes return `ok:false` and an `error` object, and the command also reports failure. `logs --follow` uses a live bounded subscription; restarting the subscription command first retrieves current status.

## Sources and Metadata

Recognized components are `venera_d3d11`, `venera_d3d12`, `mesa`, `venus`, `dxvk`, `vkd3d`, `vkd3d-shader`, `kernel`, and `service`. Venus uses the `MESA-VIRTIO` tag. Service/helper logs are identified by actual executable paths in the installation directory. Kernel records are classified by recognized VioGpu text sources.

DXVK/vkd3d `VDBG1` records are reassembled by producer, thread, sequence, and fragment number from the native adapter. Original bytes are concatenated and decoded as strict UTF-8. Ordinary DBWIN content is decoded as UTF-8 first; other bytes are converted using the current Windows ANSI code page. Complete text is retained in `text`, and the original level in `raw_level`. Sources without a level use `unknown`; available process creation times use decimal FILETIME. ETW `source_time` is converted from the system timestamp to UTC with millisecond display precision. Other unknown fields use the shared protocol's null values.

The log send queue budget is 2048 records and 8 MiB; the session handoff queue holds 2048 records; the control queue holds 64 records. Native reassembly holds 128 records and 1 MiB, with a 5-second reassembly wait. Queue eviction, native-record errors, and link interruptions generate corresponding `gap` records. Control messages take priority between complete log lines.

The service receives process creation and exit events in its own ETW session and maintains process creation times, parents, sessions, and full executable paths. Historical parent-child relationships survive parent exit; reused PIDs match separately by creation time. A process snapshot every two seconds supplements current process information. Records with descendant rules can wait for process events to establish identity. The wait queue holds 256 records, each at most 4092 bytes, with a 5-second deadline. Status queries expose process-event processing and observed ETW losses.

## Build

Run in the workspace containing `guest`:

```sh
nix develop --command cargo test --manifest-path debug-channel/Cargo.toml -p venera-debug-guest
nix develop --command cargo build --manifest-path debug-channel/Cargo.toml -p venera-debug-guest --target x86_64-pc-windows-gnu
```

The Windows GNU artifact is named `venera-debug-guest.exe`.
