#!/usr/bin/env bash
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
if [[ $# != 1 ]]; then echo "Usage: $0 /path/to/alixer.rpm" >&2; exit 2; fi
package="$(realpath -- "$1")"
[[ "$(rpm -qp --qf '%{NAME}' "$package")" == alixer ]]
mkdir -p "$root/target/rpm"
extracted="$(mktemp -d "$root/target/rpm/check.XXXXXX")"
rpm2cpio "$package" | (cd "$extracted" && cpio -idm --quiet)
for file in usr/bin/alixer usr/bin/alixerd usr/bin/alixerctl \
    usr/lib/systemd/user/alixer.service usr/lib/systemd/user-preset/90-alixer.preset \
    usr/share/wireplumber/scripts/alixer-guard.lua \
    usr/share/alixer/wireplumber/90-alixer.conf \
    usr/share/applications/org.alixer.Alixer.desktop; do
    test -f "$extracted/$file"
done
! test -e "$extracted/usr/share/wireplumber/wireplumber.conf.d/90-alixer.conf"
grep -qx 'ExecStart="/usr/bin/alixerd"' "$extracted/usr/lib/systemd/user/alixer.service"
desktop-file-validate "$extracted/usr/share/applications/org.alixer.Alixer.desktop"
"$extracted/usr/bin/alixer" --help
export ALIXER_TEST_BIN_DIR="$extracted/usr/bin"
python3 "$root/tests/runtime.py" --offline
python3 "$root/tests/runtime.py"
echo "PASS: RPM payload, desktop entry, packaged activation, persistence and private live audio"
echo "Extracted test files: $extracted"
