#!/usr/bin/env bash
# Builds release binaries for this host and packs them with the docs:
# dist/anvil-<version>-<target>.tar.gz (.zip on Windows) and a .sha256 beside it.
# The binaries build native code for the target they were built on; that needs a linker there
# (the Visual Studio C++ build tools on Windows, a C compiler on Linux and macOS).
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
target=$(rustc -vV | sed -n 's/^host: //p')
cargo build --release --locked -p anvil_cli -p anvil_mcp

ext=""
case "$target" in *windows*) ext=".exe" ;; esac
name="anvil-$version-$target"
stage="dist/$name"
rm -rf "$stage"
mkdir -p "$stage/docs"
cp "target/release/anvil$ext" "target/release/anvil-mcp$ext" "$stage/"
cp README.md LICENSE-MIT LICENSE-APACHE "$stage/"
cp docs/SPEC.md "$stage/docs/"
"$stage/anvil$ext" --version

cd dist
if [ -n "$ext" ]; then
    archive="$name.zip"
    rm -f "$archive"
    powershell -NoProfile -Command "Compress-Archive -Path '$name' -DestinationPath '$archive'"
else
    archive="$name.tar.gz"
    tar -czf "$archive" "$name"
fi
if command -v sha256sum >/dev/null; then
    sha256sum "$archive" > "$archive.sha256"
else
    shasum -a 256 "$archive" > "$archive.sha256"
fi
echo "dist/$archive"
