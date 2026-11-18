# Releasing hprof-analyzer

## Overview

Releases are tag-triggered. Pushing a `v*` tag starts a CI pipeline that:

1. Builds release binaries for all platforms
2. Publishes to crates.io
3. Creates a GitHub Release with all binaries attached
4. Opens a PR on the Homebrew tap with updated SHA256 checksums

The rolling nightly build runs separately on every push to `main`.

## Steps to cut a release

### 1. Update `Cargo.toml`

Bump `version` in `[workspace.package]`:

```toml
[workspace.package]
version = "0.3.6"
```

Both `hprof-analyzer` and `hprof-redact` inherit this via `version.workspace = true`.

### 2. Update `CHANGELOG.md`

Move the `## [Unreleased]` items into a new dated section and fill it out
completely — CI extracts this section verbatim as the GitHub Release body,
prepended with an install/quick-start preamble:

```markdown
## [0.3.6] — 2026-11-25

### Added
…

### Fixed
…
```

### 3. Commit

```sh
git add Cargo.toml CHANGELOG.md
git commit -m "chore: release v0.3.6"
```

### 4. Tag and push

```sh
git tag -a v0.3.6 -m "v0.3.6"
git push origin main --tags
```

Pushing the tag triggers the CI release pipeline. No manual steps after this.

## What CI does on a `v*` tag

| Job | Trigger | What it does |
|-----|---------|--------------|
| `build-test` | always | Runs fmt, clippy, tests — must pass first |
| `package-linux-gnu` | tag or main push | Builds `x86_64-unknown-linux-gnu` + `hprof-redact`, packages `.tar.gz` |
| `build-release` | tag or main push | Cross-compiles all other targets (macOS arm64/x86, Linux arm64, musl, Windows) |
| `release-tag` | **tag only** | Publishes to crates.io; creates GitHub Release with all artifacts |
| `update-homebrew-tap` | **tag only** | Computes SHA256s, updates `Formula/hprof-analyzer.rb` in the tap, opens a PR |

## Homebrew tap PR

After the release, CI opens a PR on
[parttimenerd/homebrew-hprof-analyzer](https://github.com/parttimenerd/homebrew-hprof-analyzer)
updating the formula's `url` and `sha256` fields. Review and merge it to make
`brew upgrade hprof-analyzer` pick up the new version.

The nightly cask (`Casks/hprof-analyzer@nightly.rb`) is updated automatically
on every `main` push — it uses `version :latest` + `sha256 :no_check` and
does not need a PR.

## Nightly builds

The nightly pipeline runs on every push to `main` (job `release-nightly`):
- Deletes the previous `nightly` GitHub Release and tag
- Rebuilds all platforms
- Re-creates the `nightly` release with the new binaries
- Updates `Casks/hprof-analyzer@nightly.rb` in the tap directly (no PR)

Install the nightly cask:
```sh
brew install --cask parttimenerd/hprof-analyzer/hprof-analyzer@nightly
```

## crates.io

`hprof-analyzer` is published to crates.io as part of `release-tag`. The job
runs `cargo publish -p hprof-analyzer --no-verify` (`--no-verify` skips a
redundant build; CI already verified it earlier in the same run).

`hprof-redact` is **not** published to crates.io — it ships only as a binary
inside the platform tarballs.

The `CARGO_REGISTRY_TOKEN` secret must be set in the repository settings.

## Versioning policy

Versions follow [Semantic Versioning](https://semver.org/) loosely:
- **Patch** (`0.3.x`): bug fixes, documentation, internal refactors
- **Minor** (`0.x.0`): new features, new CLI subcommands, new MCP tools
- **Major** (`x.0.0`): breaking changes to the JSON report schema, CLI flags,
  or MCP tool signatures

The JSON report schema version (`schema_version` in the report output) is
bumped independently when the schema changes in a backward-incompatible way.
