#!/usr/bin/env python3
"""Run rotation recovery behavior against the current Windows capture function."""
import argparse
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument("--serde-json", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
guest = Path(__file__).resolve().parents[1]
source = (guest / "src/windows/capture.rs").read_text()
start = source.index("fn rotate_sessions(")
end = source.index("\npub fn collect(", start)
template = (guest / "tests/rotation-recovery.rs.in").read_text()
args.output.mkdir(parents=True, exist_ok=True)
generated = args.output / "rotation-recovery.rs"
generated.write_text(template.replace("// PRODUCT_ROTATION_FUNCTION", source[start:end]))
binary = args.output / "rotation-recovery-tests"
subprocess.run([
    "rustc", "--edition=2021", "--test", str(generated),
    "--extern", f"serde_json={args.serde_json.resolve()}",
    "-L", f"dependency={args.serde_json.resolve().parent}", "-o", str(binary),
], check=True)
subprocess.run([str(binary), "--nocapture", "--test-threads=1"], check=True)
