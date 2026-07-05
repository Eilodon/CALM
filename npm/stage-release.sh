#!/usr/bin/env bash
# Populates each npm/calm-mcp-<platform>/calm binary from a tagged GitHub
# Release — same verified-download approach as scripts/install.sh, just
# targeting all 3 platforms instead of the caller's own — and bumps every
# package.json under npm/ to that tag's version. Doesn't publish anything;
# see npm/README.md for the manual `npm publish` steps that come after.
#
# Usage: npm/stage-release.sh vX.Y.Z
#
# Requires: curl, tar, sha256sum, jq.
set -euo pipefail

tag="${1:?usage: stage-release.sh vX.Y.Z}"
version="${tag#v}"

REPO="Eilodon/CALM"
BASE_URL="https://github.com/${REPO}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# package dir name -> release target triple
targets_pkg=(calm-mcp-linux-x64 calm-mcp-linux-arm64 calm-mcp-darwin-arm64)
targets_triple=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl aarch64-apple-darwin)

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

curl -fsSL -o "${tmp_dir}/SHA256SUMS" "${BASE_URL}/releases/download/${tag}/SHA256SUMS"

for i in "${!targets_pkg[@]}"; do
  pkg="${targets_pkg[$i]}"
  target="${targets_triple[$i]}"
  asset="calm-${target}.tar.gz"
  echo "== ${pkg} (${target}) =="

  curl -fsSL -o "${tmp_dir}/${asset}" "${BASE_URL}/releases/download/${tag}/${asset}"
  (cd "$tmp_dir" && grep " ${asset}\$" SHA256SUMS | sha256sum -c -)

  tar -xzf "${tmp_dir}/${asset}" -C "$tmp_dir" calm
  mv "${tmp_dir}/calm" "${here}/${pkg}/calm"
  chmod +x "${here}/${pkg}/calm"
done

for pkg_json in "${here}"/calm-mcp-*/package.json "${here}/calm-mcp/package.json"; do
  jq --arg v "$version" '.version = $v' "$pkg_json" > "${pkg_json}.tmp"
  mv "${pkg_json}.tmp" "$pkg_json"
done
# root wrapper's optionalDependencies pin exact versions of the platform
# packages too — bump those alongside its own version field above.
jq --arg v "$version" '.optionalDependencies |= with_entries(.value = $v)' \
  "${here}/calm-mcp/package.json" > "${here}/calm-mcp/package.json.tmp"
mv "${here}/calm-mcp/package.json.tmp" "${here}/calm-mcp/package.json"

echo
echo "Staged ${tag} (version ${version}). Publish in this order (see npm/README.md):"
echo "  cd npm/calm-mcp-linux-x64    && npm publish --access public"
echo "  cd npm/calm-mcp-linux-arm64  && npm publish --access public"
echo "  cd npm/calm-mcp-darwin-arm64 && npm publish --access public"
echo "  cd npm/calm-mcp              && npm publish --access public"
