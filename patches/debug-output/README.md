# Debug output experiment

The DXVK and vkd3d-proton patches send accepted log messages to Windows debug output. CMake prepares source copies under `build/win/dxvk-debug-output/source` and `build/win/vkd3d-proton-debug-output/source`, then builds their Meson libraries. Existing engine log destinations continue receiving their usual messages.

## Source coverage

The KMD's central debug-print function attaches `VioGpu: ` while retaining the producer's component and level filtering. The guest classifies these kernel messages as `kernel` with level `unknown`.

Mesa's `0011-util-identify-windows-debug-output.patch` attaches `MESA: ` at the Windows debug-output branch of `os_log_message`. Messages already starting with `MESA: ` or `MESA-VIRTIO: ` retain their existing identifier. The original message bytes follow the added identifier, and file/console destinations receive the original message. The guest recognizes these identifiers as `mesa` and `venus`, respectively. `icd/build.sh` applies the numbered patch to its exported source tree for both architectures.

For validation, rebuild `build/kmd/signed/viogpu3d.sys` and the four Mesa DLLs with `icd/build.sh`. Capture an adapter startup or graphics workload and check that KMD messages begin with `VioGpu: ` and Mesa utility messages begin with `MESA: `. Compare file/console output against the message following that identifier. Exercise an already identified Mesa message and confirm its existing identifier occurs once.

Mesa's Windows utility logger uses `OutputDebugStringA`. Its `_debug_vprintf` caller accumulates text in a static 4096-byte buffer, with capacity for 4095 text bytes and a terminator, and emits at a newline or when formatting exceeds available space. The utility destination adds six bytes to an unmarked message. Windows measurements should cover ANSI/UTF-8 interpretation, multiline text, near-4096-byte messages, end markers and concurrent producers. Record received lengths and exact bytes to characterize formatting and DBWIN transport limits.

## Build and checks

```sh
nix develop --command cmake -S . -B build/win -DCMAKE_TOOLCHAIN_FILE=cmake/mingw-w64.cmake
nix develop --command cmake --build build/win --target debug-output-probe dxvk-build vkd3d-proton-build
```

The probe's portable encoding checks can also run with a host C++20 compiler:

```sh
c++ -std=c++20 tests/win/debug-output-probe.cpp -o debug-output-probe
./debug-output-probe --self-test
```

Expected result: `self-test=passed`. These checks exercise UTF-8 byte encoding, two-part framing with literal expected records, and empty messages.

## Windows observations

Open two terminals in `build\win\tests\win`. Run the receiver first:

```powershell
.\debug-output-probe.exe --receive 60 > capture.txt
```

Run one producer mode per capture:

```powershell
.\debug-output-probe.exe --self-test
.\debug-output-probe.exe --emit utf8
.\debug-output-probe.exe --emit wide
.\debug-output-probe.exe --emit framed
```

The session helper and this receiver share the session's DBWIN objects. For a standalone receiver experiment, stop the `VeneraDebugGuest` service from an elevated terminal with `Stop-Service VeneraDebugGuest`, then start the receiver. Restore the regular collector afterward with `Start-Service VeneraDebugGuest`. The Panel subscribes to the service's captured records. An existing collector produces a `receiver-start` diagnostic identifying the object and Windows error.

Each mode sends Chinese text and an emoji, three lines with mixed line endings, boundary messages of 4090, 4091, 4092, 4095, 4096, 4097 and 8192 bytes, and four concurrent threads sending sequence numbers 0 through 15. Boundary messages begin with `boundary=SIZE:` and end with `:END\n`. Each producer reports message size and the measured total time inside its debug-output calls, in microseconds. Compare timings with the standalone receiver active, with the service collector active, and with both stopped. A framed logical message can involve several calls.

The receiver prints process ID, captured byte count, terminator status and exact hexadecimal bytes. Compare the unicode sample against UTF-8 `70726f626520756e69636f64653a20e4b8ade6968720f09f98800a`; compare multiline bytes against `70726f6265206c696e6520310a70726f6265206c696e6520320d0a70726f6265206c696e6520330a`. For concurrent output, check every `(thread, sequence)` pair exactly once. For boundary cases, inspect length, end marker and fragment completeness. Record Windows version, active collector/debugger, producer mode and timings with each capture. Windows captures determine transport completeness and the preferred experimental framing.

## Adapter logger runtime media

`tests/win/adapter-logger-runtime.cpp` exercises the linked DXVK `Logger::log` and, through `adapter-logger-vkd3d.c`, the linked vkd3d debug API. The two executables link the existing patched Meson archives `libutil.a` and `libvkd3d_common.a`, including their original environment and platform helpers. The DXVK executable supplies its application-owned logger instance. The vkd3d bridge selects API/shader channels, passes a literal function name and calls the producer's flush API.

Build from the workspace root using the command recorded with the media:

```sh
nix develop -c bash build/adapter-runtime-media/build-adapters.sh
```

The media is `build/adapter-runtime-media/venera-adapter-runtime.iso`. After mounting it in Windows, run the following from its drive root with the panel collector and host recorder active:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\Run-AdapterRuntime.ps1
```

The PowerShell 5.1 script selects `info` through the producers' environment settings, selects ordinary vkd3d logging, and writes producer files under `%TEMP%\venera-adapter-runtime`. It restores the previous process environment afterward. `run-results.json` records each executable's process ID, UTC start, exit result and elapsed milliseconds. The directory also contains captured console output, producer files and `literal-expectations.json`.

Each executable sends 67 logical messages: one Chinese/emoji message, one three-line message, one 8192-byte input and four threads each sending sequence numbers 0–15. DXVK produces 67 `dxvk` messages. vkd3d produces 34 `vkd3d` and 33 `vkd3d-shader` messages, with `adapter_runtime: ` preceding each input. Both use level and raw level `info`; the expected debug-output fragment count is 72 per executable. The 8192-byte input ends in `:END\n`; its reconstructed vkd3d payload is 8209 bytes including the function prefix.

The expectations JSON contains the fully expanded text, UTF-8 byte count, channel, component, level, raw level and expected occurrence count for each record. For host SQLite comparison, select `events.message` rows of type `log` from the run's process IDs and compare the multiset of `(component, level, raw_level, text)` with these entries. Require one occurrence of every entry; thread interleaving can change row order. Capture `gap` records and incomplete assemblies alongside the comparison. Windows execution and transport measurements accompany the mounted-media experiment.

## Engine record format

Each `OutputDebugStringA` call contains one ASCII record, terminated by NUL:

```text
VDBG1|producer|level|channel|tid|sequence|part|parts|hex
```

`producer` is `dxvk`, `vkd3d-proton`, or `probe`. DXVK retains levels `trace`, `debug`, `info`, `warn`, `err`, with channel `default`. vkd3d-proton retains levels `none`, `err`, `info`, `fixme`, `warn`, `trace`, with channels `api` and `shader`. `tid` is the Windows thread ID. Sequence numbers are unsigned decimal 32-bit counters local to each producer module, wrapping naturally. `part` and `parts` are one-based decimal fragment index and total count. All numeric fields use decimal ASCII. `hex` uses lowercase hexadecimal, two characters per original byte, with at most 1400 original bytes per fragment. Records fit comfortably in the 4092-byte DBWIN text area.

Use DBWIN process ID together with producer, thread ID and sequence to associate fragments; verify consistent metadata, collect every part, concatenate decoded bytes in part order, then decode UTF-8. UTF-8 code points can straddle fragments. Represent incomplete messages as partial captures. A sample single-part DXVK message `hello\n`, at thread 42 and sequence 7, is:

```text
VDBG1|dxvk|info|default|42|7|1|1|68656c6c6f0a
```

DXVK encodes the original complete message; vkd3d-proton encodes `function: ` followed by the fully formatted message. Line endings remain original bytes. Hex framing also represents embedded NUL bytes and allows validation of UTF-8 after reassembly. The DBWIN process ID supplies process attribution. The probe uses `info` and `default`; its self-test fixtures use thread 0 and sequence 1.

For engine verification, capture an existing D3D11/D3D12 probe run with the panel collector, and compare corresponding original log messages and reconstructed records. Exercise vkd3d-proton's ordinary, buffered and Wine logging modes with the producer settings used by the experiment, recording original severity and channel for each captured record.
