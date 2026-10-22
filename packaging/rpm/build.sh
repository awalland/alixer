#!/usr/bin/env bash
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
mode=build
rpm_options=()
for arg in "$@"; do
    case "$arg" in
        --prepare-only) mode=prepare ;;
        --skip-build-deps) rpm_options+=(--nodeps) ;;
        *) echo "Usage: $0 [--prepare-only] [--skip-build-deps]" >&2; exit 2 ;;
    esac
done
for tool in cargo rustc jq rg tar zstd rpmbuild; do command -v "$tool" >/dev/null; done
top="$root/target/rpm"
mkdir -p "$top"/{SOURCES,SPECS,BUILD,BUILDROOT,RPMS,SRPMS}
stage="$(mktemp -d "$top/staging.XXXXXX")"
cargo metadata --no-deps --locked --format-version=1 > "$stage/workspace.json"
version="$(jq -r '.packages[] | select(.name=="alixer") | .version' "$stage/workspace.json")"
echo "Preparing Alixer $version and locked dependencies..."
cargo vendor --locked --versioned-dirs "$stage/vendor" > "$stage/vendor-config.toml"
sed -i 's|^directory = .*|directory = "vendor"|' "$stage/vendor-config.toml"
cargo metadata --locked --offline --format-version=1 > "$stage/dependencies.json"
mkdir -p "$stage/third-party-licenses"
jq -r '.packages[] | select(.source != null) | "\(.name) \(.version): \(.license // "UNKNOWN")"' \
    "$stage/dependencies.json" > "$stage/third-party-licenses/SUMMARY.txt"
# Keep verbatim notices, including the font licenses bundled by egui.
shopt -s nocasematch
while IFS= read -r -d '' file; do
    case "$(basename -- "$file")" in
        LICENSE*|license*|LICENCE*|COPYING*|NOTICE*|COPYRIGHT*|OFL*|UFL*|UNLICENSE*)
            relative="${file#"$stage/vendor/"}"
            mkdir -p "$stage/third-party-licenses/$(dirname -- "$relative")"
            cp -- "$file" "$stage/third-party-licenses/$relative"
            ;;
    esac
done < <(rg --files --hidden --no-ignore -0 "$stage/vendor")
shopt -u nocasematch
licenses="$(jq -r '[.packages[].license // "UNKNOWN" | gsub("\\s*/\\s*"; " OR ") | gsub("\\s+"; " ")] | unique | map("(" + . + ")") | join(" AND ")' "$stage/dependencies.json")"
if [[ "$licenses" == *UNKNOWN* ]]; then echo 'A dependency has no license metadata.' >&2; exit 1; fi
sed -e "s|@VERSION@|$version|g" -e "s|@LICENSE@|$licenses|g" \
    packaging/rpm/alixer.spec.in > "$top/SPECS/alixer.spec"
tar --zstd -cf "$top/SOURCES/alixer-$version.tar.zst" \
    --transform="s|^|alixer-$version/|" --null -T <(rg --files --hidden -0 \
        Cargo.toml Cargo.lock LICENSE README.md .gitignore crates integration tests packaging img)
tar --zstd -C "$stage" -cf "$top/SOURCES/alixer-$version-vendor.tar.zst" \
    vendor vendor-config.toml third-party-licenses
echo "Prepared sources and spec under $top"
if [[ "$mode" == build ]]; then
    rpmbuild -ba "${rpm_options[@]}" --define "_topdir $top" \
        --define "alixer_jobs ${CARGO_BUILD_JOBS:-2}" "$top/SPECS/alixer.spec"
    echo "Binary RPMs: $top/RPMS"
    echo "Source RPMs: $top/SRPMS"
fi
