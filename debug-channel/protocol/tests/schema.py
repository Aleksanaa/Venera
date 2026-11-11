"""Validate the shared wire examples and representative malformed messages."""
import json
from pathlib import Path
from jsonschema import Draft202012Validator

root = Path(__file__).resolve().parents[1]
schema = json.loads((root / "schema.json").read_text())
Draft202012Validator.check_schema(schema)
validator = Draft202012Validator(schema)
vectors = [json.loads(line) for line in (root / "tests/vectors.jsonl").read_text().splitlines()]
for message in vectors:
    validator.validate(message)
panel_report = json.loads((root / "tests/panel-report.json").read_text())
validator.validate({
    "major": 1, "type": "system.info.result", "request_id": "panel-report",
    "ok": True, "collected_at": "2026-11-11T00:00:00Z", "session_id": 2,
    **panel_report,
})

valid = [json.loads(line) for line in (root / "tests/valid.jsonl").read_text().splitlines()]
for case in valid:
    validator.validate(case["input"])
    validator.validate(case["expected"])
invalid = [json.loads(line) for line in (root / "tests/invalid.jsonl").read_text().splitlines()]
for message in invalid:
    assert not validator.is_valid(message), message
print(f"schema: {len(vectors)} wire vectors, {len(valid)} valid cases, {len(invalid)} invalid cases checked")
