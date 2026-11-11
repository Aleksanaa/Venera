# Bundling the VirtIO serial driver

Venera can carry the official Windows 11 amd64 VirtIO serial driver in its installer. The guest uses this driver to reach the `org.venera.debug.0` channel configured in QEMU or libvirt.

## Source package

[`scripts/fetch-vioserial.sh`](../scripts/fetch-vioserial.sh) selects virtio-win **0.1.302** from the [official release archive](https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/archive-virtio/virtio-win-0.1.302-1/virtio-win-0.1.302.iso). It extracts `vioserial/w11/amd64` and the ISO's `virtio-win_license.txt` into `build/vioserial/package`, where the notice is named `LICENSE.txt`. The script checks the catalog and driver signatures with the Microsoft root certificate.

The package contains `vioser.inf`, `vioser.sys`, `vioser.cat` and its license notice. The installer payload includes the package under `vioserial/`, and Windows installs it through `pnputil`. The supplied catalog and driver retain their official signatures.

## Build and install

After preparing the KMD and Mesa outputs described in the [build instructions](../README.md#building), run from the repository root inside `nix develop`:

```sh
scripts/fetch-vioserial.sh
cmake -S . -B build/win -G Ninja \
  -DCMAKE_TOOLCHAIN_FILE=cmake/mingw-w64.cmake \
  -DVENERA_VIOSERIAL_DIR="$PWD/build/vioserial/package"
cmake --build build/win --target venera-setup
```

The installer is `build/win/setup/venera-setup.exe`. Its interactive installation offers the VirtIO serial driver option. Unattended installation uses:

```powershell
.\venera-setup.exe /S /with-vioserial
.\venera-setup.exe /S /skip-vioserial
```

The driver can also be installed through `.\venera-setup.exe /vioserial /S`. See the [guest setup](virtio-serial-guest.md) and [host connection instructions](virtio-serial-host.md) for the complete channel configuration.

## Redistribution notices

The selected release's `LICENSE.txt` contains BSD three-clause terms and copyright notices for Red Hat and its affiliates, Google, Virtuozzo and IBM. The terms permit source and binary redistribution with these obligations:

- Source distributions retain the copyright notice, conditions and disclaimer.
- Binary distributions reproduce the copyright notice, conditions and disclaimer in their documentation or accompanying materials.
- Use of the copyright holders' or contributors' names to endorse derived products requires prior written permission.

Publish the extracted `LICENSE.txt` unchanged alongside the installer or ISO. The GitHub build workflow includes it as `vioserial-LICENSE.txt` in the ISO, build artifact and release assets. The installer also carries this notice within its embedded payload; its temporary installation staging directory is cleaned up after installation. For example, prepare a local release directory with:

```sh
mkdir -p build/release
cp build/win/setup/venera-setup.exe build/release/
cp build/vioserial/package/LICENSE.txt build/release/vioserial-LICENSE.txt
```

The [vioserial source](https://github.com/virtio-win/kvm-guest-drivers-windows/blob/master/vioserial/sys/Driver.c) carries the corresponding three-clause notice. The release package's included notices accompany the particular binaries being distributed. Other components in Venera and in the full virtio-win ISO retain their respective licenses and copyright notices.
