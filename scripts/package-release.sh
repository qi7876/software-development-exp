#!/usr/bin/env bash
set -euo pipefail

output_directory=${1:?usage: bash scripts/package-release.sh OUTPUT_DIRECTORY}

case "$(uname -s):$(uname -m)" in
    Linux:x86_64) platform=linux-x86_64 ;;
    Darwin:arm64) platform=macos-arm64 ;;
    *) echo 'Release packages target Linux x86_64 and macOS ARM64.' >&2; exit 1 ;;
esac

cargo build --release --locked
version=$(cargo metadata --locked --no-deps --format-version 1 |
    jq -er '.packages[] | select(.name == "bak") | .version')
package="bak-v$version-$platform"
mkdir -p "$output_directory"

temporary=$(mktemp -d)
trap 'rm -r -- "$temporary"' EXIT

stage="$temporary/$package"
mkdir -p "$stage/web" "$stage/working-directory-example"
install -m 755 target/release/bak "$stage/bak"
cp LICENSE "$stage/"
cp web/{index.html,app.js,styles.css,favicon.svg} "$stage/web/"
# Reuse the configuration users copy from the README.
awk '
    /<<.BAK_EXAMPLE_CONFIG.$/ { copying = 1; next }
    /^BAK_EXAMPLE_CONFIG$/ { exit }
    copying
' README.md | jq -e 'objects' > "$stage/working-directory-example/config.json"
archive="$output_directory/$package.tar.gz"
COPYFILE_DISABLE=1 tar -czf "$archive" -C "$temporary" "$package"
echo "Created $archive"
