#!/usr/bin/env bash
# Uploads the retail patches and a fresh manifest.json to the DigitalOcean Space the launcher
# reads from (https://<bucket>.<region>.digitaloceanspaces.com/<prefix>).
#
#   ./upload-patches.sh [--config FILE] [--dry-run] [--no-manifest]
#
# Credentials and overrides are read from upload.conf next to this script (see
# upload.conf.example). SPACES_KEY / SPACES_SECRET in the environment win over the file.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
config="$here/upload.conf"
dry_run=()
manifest=1

while [[ $# -gt 0 ]]; do
    case "$1" in
        --config) config="$2"; shift 2 ;;
        --dry-run) dry_run=(--dryrun); shift ;;
        --no-manifest) manifest=0; shift ;;
        -h|--help) sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

# Parsed as KEY=VALUE lines rather than sourced, so the file can't run anything.
if [[ -f "$config" ]]; then
    while IFS= read -r line || [[ -n "$line" ]]; do
        line="${line%%#*}"
        [[ "$line" =~ ^[[:space:]]*([A-Z_]+)[[:space:]]*=[[:space:]]*(.*[^[:space:]])?[[:space:]]*$ ]] || continue
        val="${BASH_REMATCH[2]}"
        val="${val%\"}"; val="${val#\"}"; val="${val%\'}"; val="${val#\'}"
        printf -v "conf_${BASH_REMATCH[1]}" '%s' "$val"
    done < "$config"
elif [[ -z "${SPACES_KEY:-}" ]]; then
    echo "no config at $config (copy upload.conf.example)" >&2
    exit 1
fi

key="${SPACES_KEY:-${conf_SPACES_KEY:-}}"
secret="${SPACES_SECRET:-${conf_SPACES_SECRET:-}}"
bucket="${conf_SPACES_BUCKET:-axi}"
region="${conf_SPACES_REGION:-sfo3}"
prefix="${conf_SPACES_PREFIX:-ffxiv}"
patch_dir="${conf_PATCH_DIR:-../patches/ffxiv_patches}"
servers_json="${conf_SERVERS_JSON:-servers.json}"
[[ "$patch_dir" = /* ]] || patch_dir="$here/$patch_dir"
[[ "$servers_json" = /* ]] || servers_json="$here/$servers_json"

if [[ -z "$key" || -z "$secret" ]]; then
    echo "SPACES_KEY and SPACES_SECRET must be set in $config" >&2
    exit 1
fi
[[ -d "$patch_dir" ]] || { echo "patch folder not found: $patch_dir" >&2; exit 1; }
command -v aws >/dev/null || { echo "aws CLI not found (brew install awscli)" >&2; exit 1; }

export AWS_ACCESS_KEY_ID="$key" AWS_SECRET_ACCESS_KEY="$secret" AWS_DEFAULT_REGION=us-east-1
# Keep the user's ~/.aws profile out of it.
unset AWS_PROFILE AWS_SESSION_TOKEN
endpoint="https://$region.digitaloceanspaces.com"
dest="s3://$bucket/$prefix"
s3() { aws --endpoint-url "$endpoint" s3 "$@"; }

# Fail early with a readable message; aws's own error for a bad Spaces key is unhelpful.
s3 ls "s3://$bucket/" >/dev/null 2>&1 || { echo "cannot list s3://$bucket at $endpoint; check SPACES_KEY/SPACES_SECRET" >&2; exit 1; }

if (( manifest )); then
    out="$here/manifest.json"
    args=("$patch_dir" --out "$out")
    if [[ -f "$servers_json" ]]; then
        args+=(--servers "$servers_json")
    else
        echo "note: $servers_json not found; manifest will list no servers" >&2
    fi
    echo "building manifest (hashes every patch, takes a minute)…"
    (cd "$here/src-tauri" && cargo run --quiet --release --bin make-manifest -- "${args[@]}")
fi

echo "syncing patches → $dest"
s3 sync ${dry_run[@]+"${dry_run[@]}"} "$patch_dir/" "$dest/" \
    --exclude '*' --include '*/patch/*.patch' \
    --acl public-read --content-type application/octet-stream

if (( manifest )); then
    echo "uploading manifest.json"
    # no-cache so launchers see a new manifest right away; patches never change once published.
    s3 cp ${dry_run[@]+"${dry_run[@]}"} "$here/manifest.json" "$dest/manifest.json" \
        --acl public-read --content-type application/json --cache-control no-cache
fi

echo "done: https://$bucket.$region.digitaloceanspaces.com/$prefix"
