#!/bin/bash
# Installs target/recall_*.deb in fresh Ubuntu containers and runs
# `recall --self-test` on a virtual display.  Usage: scripts/docker-test.sh [image...]
set -euo pipefail
cd "$(dirname "$0")/.."
images=("${@:-ubuntu:22.04 ubuntu:24.04 ubuntu:rolling}")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cp target/recall_*_amd64.deb "$work/recall.deb"
cat > "$work/test.sh" <<'INNER'
#!/bin/bash
set -e
. /etc/os-release; echo "=== $PRETTY_NAME"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq /work/recall.deb xvfb xauth >/tmp/apt.log 2>&1 || { tail -20 /tmp/apt.log; exit 1; }
unresolved=$(LD_LIBRARY_PATH=/opt/recall/lib ldd /opt/recall/libexec/* /opt/recall/bin/pactl \
  /opt/recall/lib/gstreamer-1.0/*.so /opt/recall/lib/gdk-pixbuf-2.0/loaders/*.so 2>/dev/null | grep "not found" | sort -u || true)
[ -z "$unresolved" ] || { echo "unresolved libraries:"; echo "$unresolved"; exit 1; }
cd /tmp && HOME=/tmp xvfb-run -a recall --self-test 2>&1 | grep -vE "DRI3|^\s*$"
INNER
chmod +x "$work/test.sh"
status=0
for img in ${images[@]}; do
  docker run --rm -v "$work:/work:ro" "$img" /work/test.sh || status=1
done
exit $status
