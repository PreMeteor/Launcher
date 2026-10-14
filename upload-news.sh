#!/usr/bin/env bash
# Uploads the launcher's news (publish/news.json and its images under publish/news/) to the
# DigitalOcean Space the launcher reads from, publicly readable.
#
#   ./upload-news.sh [--config FILE] [--dry-run]
#
# Uses the same upload.conf as upload-patches.sh. SPACES_KEY / SPACES_SECRET in the environment
# win over the file.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
config="$here/upload.conf"
dry_run=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --config) config="$2"; shift 2 ;;
        --dry-run) dry_run=(--dryrun); shift ;;
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
src="$here/publish"

if [[ -z "$key" || -z "$secret" ]]; then
    echo "SPACES_KEY and SPACES_SECRET must be set in $config" >&2
    exit 1
fi
[[ -f "$src/news.json" ]] || { echo "nothing to publish: $src/news.json is missing" >&2; exit 1; }
command -v aws >/dev/null || { echo "aws CLI not found (brew install awscli)" >&2; exit 1; }

export AWS_ACCESS_KEY_ID="$key" AWS_SECRET_ACCESS_KEY="$secret" AWS_DEFAULT_REGION=us-east-1
# Keep the user's ~/.aws profile out of it.
unset AWS_PROFILE AWS_SESSION_TOKEN
endpoint="https://$region.digitaloceanspaces.com"
dest="s3://$bucket/$prefix"
s3() { aws --endpoint-url "$endpoint" s3 "$@"; }

# A broken news.json would blank the launcher's news, so check it parses first.
python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$src/news.json" \
    || { echo "publish/news.json is not valid JSON" >&2; exit 1; }

if [[ -d "$src/news" ]]; then
    echo "uploading images"
    s3 sync ${dry_run[@]+"${dry_run[@]}"} "$src/news" "$dest/news" --acl public-read --exclude ".*"
fi

echo "uploading news.json"
# no-cache so launchers pick up new posts right away.
s3 cp ${dry_run[@]+"${dry_run[@]}"} "$src/news.json" "$dest/news.json" \
    --acl public-read --content-type application/json --cache-control no-cache

echo "done: https://$bucket.$region.digitaloceanspaces.com/$prefix/news.json"
