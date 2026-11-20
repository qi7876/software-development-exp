#!/usr/bin/env bash
set -euo pipefail

output_directory=${1:?usage: bash scripts/package-release.sh OUTPUT_DIRECTORY}

case "$(uname -s):$(uname -m)" in
    Linux:x86_64) platform=linux-x86_64 ;;
    Darwin:arm64) platform=macos-arm64 ;;
    *) echo 'Release packages target Linux x86_64 and macOS ARM64.' >&2; exit 1 ;;
esac

cargo build --release --locked
version=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["package"]["version"])')
package="bak-v$version-$platform"
mkdir -p "$output_directory"

temporary=$(mktemp -d)
server_pid=
cleanup() {
    if [[ -n "$server_pid" ]]; then
        kill "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
    rm -r -- "$temporary"
}
trap cleanup EXIT

stage="$temporary/$package"
mkdir -p "$stage/web" "$stage/working-directory-example"
install -m 755 target/release/bak "$stage/bak"
cp LICENSE "$stage/"
cp web/{index.html,app.js,styles.css,favicon.svg} "$stage/web/"
cp working-directory-example/config.json "$stage/working-directory-example/"
archive="$output_directory/$package.tar.gz"
COPYFILE_DISABLE=1 tar -czf "$archive" -C "$temporary" "$package"

mkdir "$temporary/unpacked"
tar -xzf "$archive" -C "$temporary/unpacked"
unpacked="$temporary/unpacked/$package"
[[ "$("$unpacked/bak" version)" == "bak $version" ]]
work="$unpacked/working-directory-example"
python3 - "$work/config.json" <<'PY'
import json
from pathlib import Path
import sys

path = Path(sys.argv[1])
config = json.loads(path.read_text())
config["listen"] = "127.0.0.1:0"
path.write_text(json.dumps(config))
PY
"$unpacked/bak" keygen --output "$work/backup.key"
"$unpacked/bak" server start --foreground -d "$work" > "$temporary/server.log" 2>&1 &
server_pid=$!

address=
for ((attempt = 0; attempt < 100; attempt++)); do
    address=$(sed -n 's/^Listening on //p' "$temporary/server.log")
    if [[ -n "$address" ]]; then
        break
    fi
    if ! kill -0 "$server_pid" 2>/dev/null; then
        break
    fi
    sleep 0.1
done
if [[ -z "$address" ]]; then
    cat "$temporary/server.log" >&2
    exit 1
fi
curl --fail --silent --show-error --max-time 5 "http://$address/" -o "$temporary/index.html"
cmp "$unpacked/web/index.html" "$temporary/index.html"
echo "Verified $archive"
