#!/bin/sh
# SPDX-License-Identifier: MIT
# Builds yttrium's Mesa drivers for the guest from the pinned Mesa fork (third_party/virtio-win-mesa) with
# icd/patches/*.patch applied, cross-compiled with the dev shell's MinGW GCC: the Venus Vulkan driver
# (venera_vulkan_virtio.dll) and the OpenGL driver (viogpu_wgl.dll: WGL on zink, so OpenGL runs on Venus too), each
# for x86_64 and, for 32-bit processes, for i686 (venera_vulkan_virtio_32.dll, viogpu_wgl_32.dll).
#
#   icd/build.sh [build-dir]      (default build/icd; run inside nix develop)
#
# The submodule stays pristine: the build compiles an exported copy with the patches applied.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$root/build/icd}
mesa=$root/third_party/virtio-win-mesa
src=$out/src

# The meson options; changing them rebuilds from scratch. SPIRV-Tools only prints SPIR-V for debugging, and the
# dev shell's is a Linux library.
options="--buildtype=release -Db_ndebug=true -Dc_std=c11 \
    -Dplatforms=windows -Dvulkan-drivers=virtio -Dgallium-drivers=zink -Dgallium-yttrium=true \
    -Dopengl=true -Dgallium-wgl-dll-name=viogpu_wgl \
    -Dgles1=disabled -Dgles2=disabled -Degl=disabled -Dglx=disabled -Dgbm=disabled \
    -Dllvm=disabled -Dzlib=disabled -Dzstd=disabled -Dexpat=disabled -Dvalgrind=disabled \
    -Dlibunwind=disabled -Dbuild-tests=false -Dvideo-codecs= -Dmicrosoft-clc=disabled \
    -Dspirv-tools=disabled"

mkdir -p "$out"
stamp="$(git -C "$mesa" rev-parse HEAD) $options"
for p in "$root"/icd/patches/*.patch; do
    stamp="$stamp $(sha256sum "$p" | cut -d' ' -f1)"
done
if [ ! -f "$src/.stamp" ] || [ "$(cat "$src/.stamp")" != "$stamp" ]; then
    rm -rf "$src" "$out/meson" "$out/meson32"
    mkdir -p "$src"
    git -C "$mesa" archive HEAD | tar -x -C "$src"
    for p in "$root"/icd/patches/*.patch; do
        echo "icd: applying $(basename "$p")"
        (cd "$src" && patch -p1 --forward --quiet < "$p")
    done
    echo "$stamp" > "$src/.stamp"
fi

# d3dkmthk.h and friends, which MinGW lacks, from the SDK after MinGW's own headers (as for the UMD).
sdk_shared=$root/refs/winkits/sdk/c/Include/10.0.26100.0/shared
# meson adds ccache itself only for native compilers: name it in the cross file.
launcher=
if command -v ccache >/dev/null; then
    launcher="'ccache', "
fi

# Builds the two drivers for one architecture into $out/<build> and copies them to $out with a suffix:
#   build_arch <build> <triple> <meson cpu_family> <cpu> <mcfgthread include> <mcfgthread lib> <suffix>
# GCC from nixpkgs uses the mcfgthread model; link it and the GCC runtime statically, as the UMD does.
build_arch() {
    build=$out/$1 triple=$2 cpu_family=$3 cpu=$4 mcfg_include=$5 mcfg_lib=$6 suffix=$7
    cat > "$out/cross$suffix.ini" <<EOF
[binaries]
c = [${launcher}'$triple-gcc']
cpp = [${launcher}'$triple-g++']
ar = '$triple-ar'
strip = '$triple-strip'
windres = '$triple-windres'

[built-in options]
c_args = ['-isystem', '$mcfg_include', '-idirafter', '$sdk_shared']
cpp_args = ['-isystem', '$mcfg_include']
c_link_args = ['-L$mcfg_lib', '-static', '-static-libgcc']
cpp_link_args = ['-L$mcfg_lib', '-static', '-static-libgcc', '-static-libstdc++']

[host_machine]
system = 'windows'
cpu_family = '$cpu_family'
cpu = '$cpu'
endian = 'little'
EOF

    if [ ! -f "$build/build.ninja" ]; then
        # shellcheck disable=SC2086 # word splitting intended: the options list
        meson setup "$build" "$src" --cross-file "$out/cross$suffix.ini" $options
    fi
    ninja -C "$build" src/virtio/vulkan/libvulkan_virtio.dll src/gallium/targets/wgl/viogpu_wgl.dll
    # Installed next to venera_d3d12$suffix.dll under the name the UMD looks for.
    cp "$build/src/virtio/vulkan/libvulkan_virtio.dll" "$out/venera_vulkan_virtio$suffix.dll"
    cp "$build/src/gallium/targets/wgl/viogpu_wgl.dll" "$out/viogpu_wgl$suffix.dll"
    echo "icd: $out/venera_vulkan_virtio$suffix.dll $out/viogpu_wgl$suffix.dll"
}

build_arch meson x86_64-w64-mingw32 x86_64 x86_64 "$VENERA_MINGW_MCFG_INCLUDE" "$VENERA_MINGW_MCFG_LIB" ""
build_arch meson32 i686-w64-mingw32 x86 i686 "$VENERA_MINGW32_MCFG_INCLUDE" "$VENERA_MINGW32_MCFG_LIB" _32
