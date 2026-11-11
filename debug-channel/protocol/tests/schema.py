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

valid = [json.loads(line) for line in (root / "tests/valid.jsonl").read_text().splitlines()]
for case in valid:
    validator.validate(case["input"])
    validator.validate(case["expected"])
invalid = [json.loads(line) for line in (root / "tests/invalid.jsonl").read_text().splitlines()]
for message in invalid:
    assert not validator.is_valid(message), message
print(f"schema: {len(vectors)} wire vectors, {len(valid)} valid cases, {len(invalid)} invalid cases checked")
