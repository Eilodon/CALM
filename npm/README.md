# Publishing `@eilodon/calm-mcp` to npm

This directory scaffolds an npm distribution for `calm` alongside the existing
`scripts/install.sh` and git-clone-based `scripts/mcp-launcher.sh` paths —
see [`../docs/mcp-client-setup.md`](../docs/mcp-client-setup.md) for how all
three compare. It follows the same split every other Rust-CLI-on-npm project
uses (esbuild, swc, ripgrep wrappers): a thin JS entrypoint package
(`calm-mcp/`) plus one binary-only package per platform
(`calm-mcp-<platform>/`), wired together with `optionalDependencies` so `npm`
installs only the one matching binary for whoever's running it — no
postinstall network fetch, no arbitrary script execution.

**Publishing is now automated in CI** — `.github/workflows/release.yml`'s
`npm-publish` job runs `stage-release.sh` + `npm publish` on every `vX.Y.Z`
tag (gated on the `NPM_TOKEN` repo secret), then chains the MCP Registry
publish. The manual steps below stay valid as a first-time reference and for
out-of-band re-publishes; the very first publish (v0.1.4) was done by hand as
a deliberate human sanity check before the automation was wired in.

## Migration from `@eilodon/ci-mcp`

The old packages (`@eilodon/ci-mcp`, `@eilodon/ci-mcp-linux-x64`, etc.) have
been deprecated on npm with a pointer to the new names. If you had them
installed, uninstall and reinstall:

```bash
npm uninstall @eilodon/ci-mcp
npm install @eilodon/calm-mcp
```

## One-time setup (before the very first publish)

1. Create/sign in to an npm account that's a member of, or can create, the
   `@eilodon` org scope (`npm org` or the npm website). Scoped packages
   default to private, hence `--access public` below.
2. `npm login` locally.

## Publishing a release

Run this **after** `.github/workflows/release.yml` has finished publishing
the GitHub Release for a tag (stage-release.sh downloads from it, so the
release must already exist):

```bash
npm/stage-release.sh v0.2.0     # downloads all 3 platform binaries + bumps
                                 # every package.json under npm/ to 0.2.0
```

Then publish in this order — platform packages first, so a consumer who
installs the wrapper right after can already resolve its
`optionalDependencies`:

```bash
cd npm/calm-mcp-linux-x64    && npm publish --access public && cd -
cd npm/calm-mcp-linux-arm64  && npm publish --access public && cd -
cd npm/calm-mcp-darwin-arm64 && npm publish --access public && cd -
cd npm/calm-mcp              && npm publish --access public && cd -
```

## Deprecating the old packages (one-time, after first publish above)

```bash
npm deprecate @eilodon/ci-mcp "Renamed to @eilodon/calm-mcp — please migrate"
npm deprecate @eilodon/ci-mcp-linux-x64 "Renamed to @eilodon/calm-mcp-linux-x64"
npm deprecate @eilodon/ci-mcp-linux-arm64 "Renamed to @eilodon/calm-mcp-linux-arm64"
npm deprecate @eilodon/ci-mcp-darwin-arm64 "Renamed to @eilodon/calm-mcp-darwin-arm64"
```

## Verifying before you publish for real

`npm/stage-release.sh` only downloads and stages — it never calls `npm
publish`. Before your first real publish, sanity-check the staged package
locally:

```bash
npm/stage-release.sh v0.1.0   # or whatever tag is current
cd npm/calm-mcp && npm pack --dry-run   # shows exactly what `files` would ship
node bin/calm-mcp.js --version          # only works once a platform package's
                                        # `calm` binary is resolvable — e.g. via
                                        # `npm link` from calm-mcp-<platform>/,
                                        # or a manual node_modules symlink
```

## CI automation (now wired)

`release.yml` runs `stage-release.sh` + `npm publish` for all four packages
automatically on a `vX.Y.Z` tag push, gated on an `NPM_TOKEN` repo secret (an
npm automation token for the `@eilodon` scope). Add that secret once (Settings
→ Secrets and variables → Actions) to arm it; without it the `npm-publish` job
fails visibly on 401 while the GitHub Release itself still succeeds. The
`mcp-registry` job then registers the version metadata. The manual flow above
remains the fallback for re-publishing a version out of band.
