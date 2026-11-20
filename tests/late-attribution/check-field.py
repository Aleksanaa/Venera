#!/usr/bin/env python3
"""Compare the field pipe transcript with the Linux archive at each phase."""
import argparse
import json
from pathlib import Path
import sqlite3
import subprocess


def rows(text):
    return [json.loads(line) for line in text.splitlines() if line.strip()]


def expect(label, actual, expected):
    if actual != expected:
        raise SystemExit(f"{label}: expected {expected!r}, got {actual!r}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", type=int, choices=(2, 3))
    parser.add_argument("database", type=Path)
    parser.add_argument("pipe_transcript", type=Path)
    parser.add_argument("transport_transcript", type=Path)
    args = parser.parse_args()
    phase = args.phase
    page, ack = rows(args.pipe_transcript.read_text(encoding="utf-8-sig"))
    journal = page["range"]["journal_id"]
    cursor = {"journal_id": journal, "position": str(phase)}
    expect("named-pipe ACK", ack["pipe_ack"]["cursor"], cursor)
    trace = rows(args.transport_transcript.read_text(encoding="utf-8-sig"))
    for transport in ("pipe", "vsock"):
        expect(f"{transport} accepted ACK", {"transport": transport, "ack": {"cursor": cursor}} in trace, True)
    originals = page["records"][:2]
    expect("original bodies", [e["event"]["record"]["text"] for e in originals],
           ["A: before PID reuse", "B: after PID reuse"])
    if phase == 3:
        previous = rows(args.pipe_transcript.with_name("pipe-2.jsonl").read_text(encoding="utf-8-sig"))[0]
        expect("phase-2 originals vs. phase-3 originals", originals, previous["records"])
        revision = page["records"][2]
        expect("revision cursor", revision["cursor"], cursor)
        expect("revision kind", revision["event"]["type"], "attribution_revision")
        expect("revision target", revision["event"]["target"], {"journal_id": journal, "position": "1"})
        expect("revision key", revision["event"]["attribution"]["process"]["start_key"], "4201")
    exported = rows(subprocess.check_output([str(Path(__file__).resolve().with_name("venera-debug-host")),
                                              "export", "--database", args.database], text=True))
    history = [r["message"] for r in exported if "cursor" in r["message"]]
    expect("raw export vs. pipe (position, body, source time, identity)", history, page["records"])
    with sqlite3.connect(args.database.resolve().as_uri() + "?mode=ro", uri=True) as db:
        positions = db.execute("SELECT journal_id,position FROM history_checkpoints").fetchall()
        expect("Linux durable cursor", positions, [(journal, str(phase))])
        projected = db.execute("SELECT position,message FROM events WHERE journal_id=? ORDER BY id", (journal,)).fetchall()
    expect("positions and duplicate count", [p for p, _ in projected], [str(n) for n in range(1, phase + 1)])
    for i, (position, encoded) in enumerate(projected[:2]):
        row = json.loads(encoded)
        original = page["records"][i]["event"]["record"]
        expect(f"{position} body", row["text"], original["text"])
        expect(f"{position} source time", row["source_time"], original["source_time"])
        expect(f"{position} PID", row["process"]["pid"], 42)
        expect(f"{position} projected key", row["process"].get("start_key"), ("4201" if phase == 3 else None, "4202")[i])
        expect(f"{position} ancestors", [p["pid"] for p in row.get("ancestors") or []], ([10] if phase == 3 else [], [11])[i])
    print(f"{journal}: pipe ACK={phase}, vsock ACK={phase}, Linux durable cursor={phase}, raw records={len(history)}")


if __name__ == "__main__":
    main()
