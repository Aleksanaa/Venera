#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
# https://github.com/virtio-win/virtio-win-pkg-scripts documents the official stable ISO.
# The stable redirect selected this release on 2026-11-10.
version=0.1.302
size=877373440
base="$root/build/vioserial-cache"
archive="$base/virtio-win-$version.iso"
out="$root/build/vioserial"
url="https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/archive-virtio/virtio-win-$version-1/virtio-win-$version.iso"
mkdir -p "$base"
if [[ ! -f "$archive" || $(stat -c %s "$archive") != "$size" ]]; then
  curl --fail --location --continue-at - --output "$archive" "$url"
fi
if [[ $(stat -c %s "$archive") != "$size" ]]; then
  echo "The virtio-win $version ISO has an unexpected size" >&2
  exit 1
fi
mkdir -p "$out/package"
xorriso -osirrox on -indev "$archive" \
  -extract /vioserial/w11/amd64 "$out/package" \
  -extract /virtio-win_license.txt "$out/package/LICENSE.txt"
for name in vioser.inf vioser.sys vioser.cat LICENSE.txt; do
  test -s "$out/package/$name"
done
root_cert="$base/microsoft-root-2010.crt"
if [[ ! -s "$root_cert" ]]; then
  curl --fail --location --output "$root_cert.tmp" \
    https://www.microsoft.com/pki/certs/MicRooCerAut_2010-06-23.crt
  mv "$root_cert.tmp" "$root_cert"
fi
openssl x509 -inform DER -in "$root_cert" -out "$base/microsoft-root-2010.pem"
osslsigncode verify -CAfile "$base/microsoft-root-2010.pem" \
  -TSA-CAfile "$base/microsoft-root-2010.pem" -in "$out/package/vioser.cat"
osslsigncode verify -CAfile "$base/microsoft-root-2010.pem" \
  -TSA-CAfile "$base/microsoft-root-2010.pem" \
  -catalog "$out/package/vioser.cat" -in "$out/package/vioser.sys"
printf 'virtio-win %s\nISO: %s\nPackage: %s\n' "$version" "$url" "$out/package"
