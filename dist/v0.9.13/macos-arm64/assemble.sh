#!/bin/sh
set -eu
cd "$(dirname "$0")"
OUT="keyjitsu-v0.9.13-macos-arm64.zip"
cat "$OUT".part-* > "$OUT"
printf '%s  %s\n' 'b6810718c32526c9a5e518c5dbb87f56a41334aec9e11f3dd04c13319d8e9b6e' "$OUT" > .expected.sha256
shasum -a 256 -c .expected.sha256
rm -f .expected.sha256
echo "Built $OUT"
