# Kernel events

The Venera kernel provider is `6c5a2378-22b9-4c10-a91f-69e34c280b81`. Its version-1 events share a producer instance, sequence and source FILETIME across ETW delivery and WPP Recorder replay.

| Event ID | Event | Status |
| --- | --- | --- |
| 1 | Diagnostic message | Zero |
| 2 | Kernel initialization | ETW provider registration result |
| 3 | Display driver registration | `DxgkInitialize` result |
| 4 | Kernel unload | Zero |

Initialization, registration and unload have explicit structured events. Ordinary display-driver diagnostics use `DbgPrint` with the `VioGpu: ` prefix. The driver checks its trace level before evaluating each message's arguments. Windows applies the DEFAULT component's INFO bit (`0x8`) to these prints; the boot marker uses its existing error-inclusive mask.

## Event layout

Fields are serialized consecutively in little-endian order:

| Field | Type |
| --- | --- |
| Producer instance | UInt64, initialization interrupt time |
| Producer sequence | UInt64 |
| Source time | UInt64, FILETIME |
| Event ID | UInt16 |
| Version | UInt8 |
| Severity | UInt8 |
| Source line | UInt32 |
| Status | Int32 |
| Function | Zero-terminated ANSI string |
| Message | Zero-terminated ANSI string |

The guest preserves typed fields in the canonical record's `structured` object, together with the original transport provider, descriptor and timestamp. WPP control uses `88ff4af9-b7b4-40ff-94ad-271b4777ed97`; the paired TMF identifies the generated message GUID.

## Generated Recorder pair

`generated/recorder.tmh` supplies Microsoft's Recorder initialization and recording code. `generated/venera-recorder.tmf` supplies the matching typed formatting data and is included in the driver package beside the guest collector.

Regenerate with the existing SDK and Wine executables:

```sh
VENERA_WINE=/path/to/wine VENERA_WINKITS_DIR=/path/to/winkits \
  python3 kmd/logging/generate.py
VENERA_WINE=/path/to/wine VENERA_WINKITS_DIR=/path/to/winkits \
  python3 kmd/logging/generate.py --check
```

The script generates both files in one tracewpp invocation, verifies the message GUID and ten argument types, and compares regenerated contents in check mode. The module context and generated time/version settings are fixed for reproducibility.
