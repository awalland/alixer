#!/usr/bin/env bash
# Upload the RPMs of the current workspace version to the Gitea RPM registry.
# Reads credentials from publish.env in the repository root (0600, gitignored),
# overridable with ALIXER_PUBLISH_CONFIG or environment variables. Create it
# from publish.env.example; it needs:
#   GITEA_BASE_URL=https://git.home
#   GITEA_USERNAME=yourname
#   GITEA_PACKAGE_OWNER=packages
#   GITEA_TOKEN=...   # scoped token with write:package only
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
top="$root/target/rpm"
config="${ALIXER_PUBLISH_CONFIG:-$root/publish.env}"
if [[ -f "$config" ]]; then
    source "$config"
    perms="$(stat -c '%a' "$config")"
    [[ "$perms" == 600 ]] || echo "warning: $config is mode $perms, expected 600" >&2
else
    echo "Missing $config; copy publish.env.example to publish.env and edit it." >&2
    exit 1
fi
: "${GITEA_BASE_URL:?set GITEA_BASE_URL in $config}"
: "${GITEA_USERNAME:?set GITEA_USERNAME in $config}"
: "${GITEA_PACKAGE_OWNER:?set GITEA_PACKAGE_OWNER in $config}"
: "${GITEA_TOKEN:?set GITEA_TOKEN in $config}"
for tool in cargo curl jq; do command -v "$tool" >/dev/null; done
version="$(cargo metadata --no-deps --locked --format-version=1 \
    | jq -r '.packages[] | select(.name=="alixer") | .version')"
upload_url="${GITEA_BASE_URL%/}/api/packages/${GITEA_PACKAGE_OWNER}/rpm/upload"
rpms=()
for arch in x86_64 noarch; do
    [[ -f "$top/RPMS/$arch/alixer-$version-1.$arch.rpm" ]] && \
        rpms+=("$top/RPMS/$arch/alixer-$version-1.$arch.rpm")
done
if [[ ${#rpms[@]} -eq 0 ]]; then
    echo "No alixer-$version RPMs under $top/RPMS; run packaging/rpm/build.sh first." >&2
    exit 1
fi
# The token goes to curl over stdin so it never appears in the process list.
for rpm in "${rpms[@]}"; do
    echo "Publishing $(basename -- "$rpm") to $upload_url"
    printf 'user = "%s:%s"\n' "$GITEA_USERNAME" "$GITEA_TOKEN" | \
        curl --fail-with-body --silent --show-error --config - \
            --upload-file "$rpm" "$upload_url"
done
