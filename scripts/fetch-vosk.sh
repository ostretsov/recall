#!/bin/bash
# Downloads the prebuilt Vosk speech recognition library (libvosk.so, which
# carries Kaldi inside) into vendor/vosk.  The build links against it and the
# .deb bundles it.  Idempotent: does nothing when the right version is there.
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=0.3.45
SHA256=bbdc8ed85c43979f6443142889770ea95cbfbc56cffb5c5dcd73afa875c5fbb2
URL=https://github.com/alphacep/vosk-api/releases/download/v$VERSION/vosk-linux-x86_64-$VERSION.zip
DEST=vendor/vosk

if [ -f "$DEST/libvosk.so" ] && [ "$(cat "$DEST/VERSION" 2>/dev/null)" = "$VERSION" ]; then
  exit 0
fi
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "Downloading Vosk $VERSION ..."
curl -fsSL -o "$tmp/vosk.zip" "$URL"
echo "$SHA256  $tmp/vosk.zip" | sha256sum -c --quiet
python3 -c 'import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "$tmp/vosk.zip" "$tmp"
rm -rf "$DEST"
mkdir -p "$DEST"
cp "$tmp/vosk-linux-x86_64-$VERSION/libvosk.so" "$tmp/vosk-linux-x86_64-$VERSION/vosk_api.h" "$DEST/"
echo "$VERSION" > "$DEST/VERSION"
echo "Vosk $VERSION in $DEST"
