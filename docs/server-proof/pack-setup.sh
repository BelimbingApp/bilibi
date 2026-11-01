#!/bin/bash
# Pack a bilibi-web binary as a GitHub setup archive. Lab proof helper only.
set -euo pipefail
bin=${1:?binary}
out=${2:?archive}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp -- "$bin" "$stage/bilibi-web"
chmod 755 "$stage/bilibi-web"
(cd "$stage" && sha256sum bilibi-web > SHA256SUMS)
tar -C "$stage" -czf "$out" bilibi-web SHA256SUMS
echo "packed $(wc -c < "$out") bytes -> $out"
sha256sum "$out"
