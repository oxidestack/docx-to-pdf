#!/bin/sh
# Build the documentation site into site/dist/: the lockfile's licences are checked, npm installs exactly
# package-lock.json's tree, then the build stages README.md and docs/guide/ and fails on a broken internal link or
# anchor (astro.config.mjs). It runs where node is: `make site` runs it in the pinned node image
# (tools/lint/images.Dockerfile's `node` stage), so no host needs node, and the CI's `site` job runs it in that image
# directly.
set -eu
cd "$(dirname "$0")"
# Every package of the lockfile against deny.toml's licence allowlist, before npm fetches one (scripts/licences.mjs).
node scripts/licences.mjs
# npm's cache stays in the repository's target/, never in the source tree or a home directory.
npm ci --ignore-scripts --no-audit --no-fund --no-update-notifier --cache ../target/site/npm-cache --loglevel=error
# Astro's telemetry would send a request home on every build, and npm's update check one more.
ASTRO_TELEMETRY_DISABLED=1 NPM_CONFIG_UPDATE_NOTIFIER=false npm run build
