# Windows graphics log events

The graphics libraries and resolution tools use provider `6c5a2378-22b9-4c10-a91f-69e34c280b81`, keyword 1. IDs 5 and 6 carry D3D11 and D3D12 UMD logs. The user text descriptors below use version 1 and ETW level 4; each payload carries the emitter's original severity.

| ID | Source | Channel | Text |
|---|---|---|---|
| 7 | DXVK | `default` | Original logger message |
| 8 | vkd3d-proton | `api` or `shader` | Function, `: ` and formatted message |
| 9 | Mesa `os_log_message` | `default` | Input bytes; severity `unknown` |
| 10 | Mesa `logger_windbg` | Original tag | Formatted output; `MESA-VIRTIO` identifies Venus |
| 11 | Resolution service (`vgpusrv`) | `default` | UTF-8 formatted message; severity `unknown` |
| 12 | Resolution helper (`viogpuap`) | `default` | UTF-8 formatted message; severity `unknown` |

`engine_etw.h` encodes the same payload for 32- and 64-bit C and C++ producers. In wire order:

* Four little-endian 64-bit values: source FILETIME, process creation FILETIME, producer instance and message sequence. The instance is a boot-local LUID with bit 63 set. Each writer starts its sequence at one.
* Two little-endian 32-bit values: zero-based fragment index and fragment count.
* Two NUL-terminated byte strings: raw severity and channel.
* Fragment bytes occupy the rest of the event. A complete message can contain NUL bytes. Fragments carry up to 60 KiB of text, leaving space for ETW and source metadata inside a 64 KiB trace buffer.

The guest decodes text after all fragments of a message arrive. ETL replay rebuilds the assembly from the start of each file, including events preceding its import checkpoint. Earlier driver builds remain readable through the UMD event layouts.

Live capture and ETL replay decode the resolution messages as UTF-8. The resolution tools also append to `vgpusvp.log` in their working directory.

The source-runtime probes exercise the actual patched libraries. After building both graphics stacks, use `tests/win/build-json-probes.py --etw-fixture` and `python tests/win/engine-etw-matrix.py build/json-probes wine` to compare their file output and emitted bytes.
