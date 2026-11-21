# Bundling the VirtIO socket driver

Venera includes the Windows 11 amd64 VirtIO socket driver and Winsock provider in its installer. The guest uses the package to communicate with the host debug channel.

## Source package

[`scripts/fetch-viosock.sh`](../scripts/fetch-viosock.sh) selects virtio-win **0.1.302** from the [official release archive](https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/archive-virtio/virtio-win-0.1.302-1/virtio-win-0.1.302.iso). It prepares `build/viosock/package` from `viosock/w11/amd64` and the ISO's `virtio-win_license.txt`.

The package contains `viosock.inf`, `viosock.cat`, `viosock.sys`, `viosocklib_x64.dll`, `viosocklib_x86.dll` and `viosockwspsvc.exe`, together with `LICENSE.txt`, `viosock-LICENSE.txt` and `viosock-lib-LICENSE.txt`. The installer includes these files under `viosock/`.

## Build and install

From the repository root, use the certificate described in the [build instructions](../README.md#building):

```sh
export VENERA_KMD_SIGN_PFX=/absolute/path/to/existing-test-signing.pfx
nix develop -c scripts/build installer
```

The versioned installer, ISO and accompanying notices are in `build/release/`. To select a prepared VirtIO socket package, configure CMake inside `nix develop`:

```sh
cmake -S . -B build/win -G Ninja \
  -DCMAKE_TOOLCHAIN_FILE=cmake/mingw-w64.cmake \
  -DVENERA_VIOSOCK_DIR=/absolute/path/to/viosock/package
cmake --build build/win --target venera-setup
```

The installer is `build/win/setup/venera-setup.exe`. Its component selection includes the transport. An unattended installation can select it with:

```powershell
.\venera-setup.exe /S /components=graphics,panel,collector,transport
```

See the [guest setup](virtio-socket-guest.md) and [host connection instructions](virtio-socket-host.md) for channel configuration.

## Redistribution notices

The package includes the original virtio-win license and the viosock driver and library notices. The [driver notice](third-party/viosock-LICENSE.txt) credits Virtuozzo and Red Hat; the [library notice](third-party/viosock-lib-LICENSE.txt) credits Virtuozzo. Both contain three-clause BSD terms. The release directory and ISO include these notices as `virtio-win-LICENSE.txt`, `viosock-LICENSE.txt` and `viosock-lib-LICENSE.txt`, alongside the nlohmann/json notice.
