#!/usr/bin/env bash
set -euo pipefail
config=(--offline)
if [[ -n ${CARGO_VENDOR_DIR:-} ]]; then
    vendor=$CARGO_VENDOR_DIR
    [[ $vendor = /* ]] || vendor=$PWD/$vendor
    vendor=${vendor//\\/\\\\}
    vendor=${vendor//\"/\\\"}
    config+=(--config 'source.crates-io.replace-with="vendor"'
             --config "source.vendor.directory=\"$vendor\"")
fi
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
cargo generate-lockfile --quiet "${config[@]}"
exec cargo run --locked --quiet "${config[@]}"
