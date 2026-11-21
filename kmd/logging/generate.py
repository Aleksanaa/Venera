#!/usr/bin/env python3
"""Generate the paired kernel Recorder definitions and formatting data."""
import argparse
import os
from pathlib import Path
import re
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    here = Path(__file__).resolve().parent
    root = here.parent.parent
    sdk = Path(os.environ.get("VENERA_WINKITS_DIR", root / "refs/winkits")) / "sdk/c/bin/10.0.26100.0"
    wine = os.environ["VENERA_WINE"]
    build = root / "build/wpp-generation"
    build.mkdir(parents=True, exist_ok=True)
    windows = lambda path: "Z:" + str(path).replace("/", "\\")
    with tempfile.TemporaryDirectory(dir=build) as temporary:
        output = Path(temporary)
        env = dict(os.environ, WINEDEBUG="-all")
        env.setdefault("WINEPREFIX", str(build / "wine"))
        subprocess.run([
            wine, str(sdk / "x64/tracewpp.exe"), "-km", "-DENABLE_WPP_RECORDER=1",
            "-p:Venera", "-NoDateTime", "-NoEnvironment", "-NoWppVersion",
            "-I" + windows(sdk / "WppConfig/Rev1") + ";" + windows(here),
            "-gen:{km-default.tpl}*.tmh", "-gen:{tmf.tpl}venera-recorder.tmf",
            "-odir:" + windows(output), "-scan:recorder.cpp", "recorder.cpp",
        ], cwd=here, env=env, check=True)
        tmh = (output / "recorder.tmh").read_text()
        tmf = (output / "venera-recorder.tmf").read_text()
        guid = tmf.split()[0]
        if not re.fullmatch(r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}", guid) or guid not in tmh:
            raise RuntimeError("Recorder message GUID differs between generated files")
        fields = re.findall(r"^(\w+), (\w+) -- \d+$", tmf, re.M)
        expected = [
            ("producerInstance", "ItemULongLong"), ("sequence", "ItemULongLong"),
            ("sourceTime", "ItemULongLong"), ("eventId", "ItemShort"),
            ("version", "ItemUChar"), ("level", "ItemUChar"),
            ("line", "ItemLong"), ("status", "ItemLong"),
            ("function", "ItemString"), ("message", "ItemString"),
        ]
        if fields != expected or "WppAutoLogTrace" not in tmh or "WppAutoLogStart" not in tmh:
            raise RuntimeError(f"Recorder schema differs from the event decoder: {fields!r}")
        for name, content in [("recorder.tmh", tmh), ("venera-recorder.tmf", tmf)]:
            if str(root) in content or "Z:\\" in content:
                raise RuntimeError(f"Machine path in generated {name}")
            destination = here / "generated" / name
            if args.check:
                if destination.read_text() != content:
                    raise RuntimeError(f"Regenerate {destination}")
            else:
                destination.parent.mkdir(exist_ok=True)
                destination.write_text(content)
        print(f"Recorder pair verified: {guid}, {len(fields)} typed fields")


if __name__ == "__main__":
    main()
