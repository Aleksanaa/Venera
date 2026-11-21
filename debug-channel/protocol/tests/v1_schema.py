"""Validate generated discovery documents and shared codec examples."""
import json
from copy import deepcopy
from pathlib import Path

from jsonschema import Draft7Validator
from referencing import Registry, Resource
from referencing.jsonschema import DRAFT7

root = Path(__file__).resolve().parents[1]
fixtures = root / "tests/v1-fixtures"
meta = json.loads((fixtures / "openrpc-1.14.9.json").read_text())
schema_meta = json.loads((fixtures / "json-schema-tools-1.8.3.json").read_text())
registry = Registry().with_resources([
    (uri, Resource.from_contents(schema_meta, default_specification=DRAFT7))
    for uri in ("https://meta.json-schema.tools", "https://meta.json-schema.tools/")
])
# The 1.4.1 profile retains the official structure and extends its version enum.
profile = deepcopy(meta)
profile["properties"]["openrpc"]["enum"].append("1.4.1")
validator = Draft7Validator(profile, registry=registry)
samples = json.loads((root / "tests/v1_samples.json").read_text())


def check_refs(value, document):
    if isinstance(value, dict):
        if "$ref" in value:
            reference = value["$ref"]
            assert reference.startswith("#/components/schemas/"), reference
            target = document
            for key in reference[2:].split("/"):
                target = target[key.replace("~1", "/").replace("~0", "~")]
        for child in value.values():
            check_refs(child, document)
    elif isinstance(value, list):
        for child in value:
            check_refs(child, document)


checked = 0
for role in ("guest", "host"):
    document = json.loads((root / f"v1/{role}.openrpc.json").read_text())
    validator.validate(document)
    check_refs(document, document)
    names = [method["name"] for method in document["methods"]]
    assert len(names) == len(set(names)), names
    for method in document["methods"]:
        params = method["params"]
        names = [param["name"] for param in params]
        assert len(names) == len(set(names)), names
        required = [param.get("required", False) for param in params]
        assert required == sorted(required, reverse=True), method["name"]
    for component in document["components"]["schemas"].values():
        Draft7Validator.check_schema(component)
    for sample in samples:
        if sample["component"] not in document["components"]["schemas"]:
            continue
        schema = {"$ref": f"#/components/schemas/{sample['component']}",
                  "components": document["components"]}
        assert Draft7Validator(schema).is_valid(sample["value"]) == sample["schema"], sample
        checked += 1
    discovery = {"$ref": "#/components/schemas/OutputDiscovery",
                 "components": document["components"]}
    Draft7Validator(discovery).validate(document)
assert checked >= len(samples), checked
print(f"v1 schema: 2 discovery documents and {checked} shared sample checks passed")
