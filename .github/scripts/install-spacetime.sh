#!/usr/bin/env bash
set -euo pipefail

: "${RUNNER_TEMP:?This installer requires a GitHub Actions runner}"
: "${GITHUB_PATH:?This installer requires GitHub Actions}"

# Download the exact CLI release directly. The version manager's legacy S3
# mirror can return 403 even while the official GitHub asset is available.
cli_version=2.0.5
case "$(uname -s):$(uname -m)" in
  Darwin:arm64|Darwin:aarch64) cli_target=aarch64-apple-darwin ;;
  Darwin:x86_64) cli_target=x86_64-apple-darwin ;;
  Linux:aarch64|Linux:arm64) cli_target=aarch64-unknown-linux-gnu ;;
  Linux:x86_64) cli_target=x86_64-unknown-linux-gnu ;;
  *) echo 'Unsupported SpacetimeDB CLI host platform' >&2; exit 1 ;;
esac

cli_dir="$RUNNER_TEMP/spacetime-cli-$cli_version"
mkdir -p "$cli_dir" "$HOME/.local/bin"
curl --fail --location --silent --show-error --retry 3 --retry-all-errors \
  --connect-timeout 15 --max-time 120 \
  "https://github.com/clockworklabs/SpacetimeDB/releases/download/v$cli_version/spacetime-$cli_target.tar.gz" \
  --output "$cli_dir/cli.tar.gz"
tar -xzf "$cli_dir/cli.tar.gz" -C "$cli_dir" spacetimedb-cli spacetimedb-standalone
chmod +x "$cli_dir/spacetimedb-cli" "$cli_dir/spacetimedb-standalone"
ln -sf "$cli_dir/spacetimedb-cli" "$HOME/.local/bin/spacetime"
# macOS resolves the standalone schema helper relative to the invoked CLI path.
ln -sf "$cli_dir/spacetimedb-standalone" "$HOME/.local/bin/spacetimedb-standalone"
echo "$HOME/.local/bin" >> "$GITHUB_PATH"
"$HOME/.local/bin/spacetime" --version
"$HOME/.local/bin/spacetimedb-standalone" --version
