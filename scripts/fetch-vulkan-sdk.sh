#!/bin/bash
# Downloads the parts of the LunarG Vulkan SDK that building whisper.cpp's
# Vulkan (GPU) backend needs into vendor/vulkan-sdk: the glslc shader
# compiler (not packaged by Ubuntu 22.04), headers and the loader to link
# against.  Only needed at build time; at run time the system's libvulkan1
# and graphics driver are used.  Idempotent.
#
#   export VULKAN_SDK=$PWD/vendor/vulkan-sdk   (the Makefile does this)
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=1.3.296.0
SHA256=79b0a1593dadc46180526250836f3e53688a9a5fb42a0e5859eb72316dc4d53e
URL=https://sdk.lunarg.com/sdk/download/$VERSION/linux/vulkansdk-linux-x86_64-$VERSION.tar.xz
DEST=vendor/vulkan-sdk

if [ -x "$DEST/bin/glslc" ] && [ "$(cat "$DEST/VERSION" 2>/dev/null)" = "$VERSION" ]; then
  exit 0
fi
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "Downloading the Vulkan SDK $VERSION (~260 MB, keeps ~40 MB) ..."
curl -fsSL -o "$tmp/sdk.tar.xz" "$URL"
sum=$(sha256sum "$tmp/sdk.tar.xz" | cut -d' ' -f1)
if [ "$sum" != "$SHA256" ]; then
  echo "checksum mismatch: $sum" >&2; exit 1
fi
tar -xJf "$tmp/sdk.tar.xz" -C "$tmp" \
  "$VERSION/x86_64/bin/glslc" "$VERSION/x86_64/include" "$VERSION/x86_64/lib"
rm -rf "$DEST"
mkdir -p "$DEST"
cp -a "$tmp/$VERSION/x86_64/bin" "$tmp/$VERSION/x86_64/include" "$DEST/"
mkdir -p "$DEST/lib"
cp -a "$tmp/$VERSION/x86_64/lib"/libvulkan.so* "$DEST/lib/"
echo "$VERSION" > "$DEST/VERSION"
echo "Vulkan SDK parts in $DEST ($(du -sh "$DEST" | cut -f1))"
