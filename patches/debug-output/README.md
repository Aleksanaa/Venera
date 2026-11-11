# Debug Output Integration

The DXVK and vkd3d-proton patches send accepted log messages to Windows debug output. CMake prepares source copies under `build/win/dxvk-debug-output/source` and `build/win/vkd3d-proton-debug-output/source`, then builds their Meson libraries. Existing engine log destinations continue receiving their usual messages.

## Source coverage

The KMD's central debug-print function attaches `VioGpu: ` while retaining the producer's component and level filtering. The guest classifies these kernel messages as `kernel` with level `unknown`.

Mesa's `0011-util-identify-windows-debug-output.patch` attaches `MESA: ` at the Windows debug-output branch of `os_log_message`. Messages already starting with `MESA: ` or `MESA-VIRTIO: ` retain their existing identifier. The original message bytes follow the added identifier, and file/console destinations receive the original message. The guest recognizes these identifiers as `mesa` and `venus`, respectively. `icd/build.sh` applies the numbered patch to its exported source tree for both architectures.

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

