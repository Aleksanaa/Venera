#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess


def run(args, **kwargs):
    print(shlex.join(map(str, args)), flush=True)
    subprocess.run(list(map(str, args)), check=True, **kwargs)


parser = argparse.ArgumentParser(description="Build Windows source-runtime and channel-stress probes")
parser.add_argument("--build-dir", type=Path, default=Path("build/win"))
parser.add_argument("--mesa-build-dir", type=Path, default=Path("build/icd/meson"))
parser.add_argument("--output-dir", type=Path, default=Path("build/json-probes"))
args = parser.parse_args()
root = Path(__file__).resolve().parents[2]
sources = root / "tests/win"
out = args.output_dir.resolve()
out.mkdir(parents=True, exist_ok=True)
dxvk = args.build_dir.resolve() / "dxvk-debug-output"
vkd3d = args.build_dir.resolve() / "vkd3d-proton-debug-output"
mesa = args.mesa_build_dir.resolve()
json_flags = shlex.split(subprocess.check_output(["pkg-config", "--cflags", "nlohmann_json"], text=True))
compile_flags = ["-O2", "-isystem", os.environ["VENERA_MINGW_MCFG_INCLUDE"]]
link_flags = ["-static", "-static-libgcc", "-static-libstdc++", "-L" + os.environ["VENERA_MINGW_MCFG_LIB"]]
cxx = ["x86_64-w64-mingw32-g++", "-std=c++20", *compile_flags, *json_flags]
for name in ("source-runtime-venera", "channel-stress-venera"):
    run([*cxx, "-I" + str(root / "umd/common"), sources / (name + ".cpp"),
         root / "umd/common/log.cpp", root / "umd/common/stacks.cpp", *link_flags,
         "-ladvapi32", "-o", out / (name + ".exe")])
run([*cxx, "-DUNICODE", "-D_UNICODE", "-I" + str(root / "kmd/yttrium/viogpu/shared"),
     sources / "source-runtime-resolution.cpp", root / "kmd/yttrium/viogpu/shared/log.cpp",
     *link_flags, "-o", out / "source-runtime-resolution.exe"])
for name in ("source-runtime-dxvk", "channel-stress-dxvk"):
    run([*cxx, "-DNOMINMAX", "-D_WIN32_WINNT=0xa00", "-I" + str(dxvk / "source/src/util"),
         "-I" + str(dxvk / "source/include"), sources / (name + ".cpp"),
         dxvk / "meson/src/util/libutil.a", *link_flags, "-o", out / (name + ".exe")])
helper = out / "source-runtime-json.o"
run([*cxx, "-c", sources / "source-runtime-json.cpp", "-o", helper])
vkd3d_object = out / "source-runtime-vkd3d.o"
run(["x86_64-w64-mingw32-gcc", "-std=c11", *compile_flags, "-D_WIN32_WINNT=0xa00",
     "-DVKD3D_NO_TRACE_MESSAGES", *["-I" + str(vkd3d / path) for path in
        ("source/include/private", "source/include", "source/khronos/Vulkan-Headers/include", "meson/include")],
     "-c", sources / "source-runtime-vkd3d.c", "-o", vkd3d_object])
run([*cxx, vkd3d_object, helper, vkd3d / "meson/libs/vkd3d-common/libvkd3d_common.a",
     *link_flags, "-lversion", "-lpathcch", "-o", out / "source-runtime-vkd3d.exe"])
commands = json.loads((mesa / "compile_commands.json").read_text())
entry = next(command for command in commands if command["file"].endswith("/util/log.c"))
original = shlex.split(entry["command"])
original = original[original.index("x86_64-w64-mingw32-gcc"):]
mesa_flags = []
index = 0
while index < len(original):
    if original[index] in ("-MQ", "-MF", "-o", "-c"):
        index += 2
    elif original[index] == "-MD":
        index += 1
    else:
        mesa_flags.append(original[index])
        index += 1
mesa_object = out / "source-runtime-mesa.o"
run([*mesa_flags, "-c", sources / "source-runtime-mesa.c", "-o", mesa_object], cwd=mesa)
run([*cxx, mesa_object, helper, *link_flags, "-Wl,--start-group", mesa / "src/util/libmesa_util.a",
     mesa / "src/c11/impl/libmesa_util_c11.a", "-Wl,--end-group", "-lversion", "-lwinmm",
     "-lws2_32", "-lsynchronization", "-o", out / "source-runtime-mesa.exe"])
