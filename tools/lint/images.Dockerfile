# The tool images of `make lint`, one stage per image, each pinned by the digest of its multi-arch
# index, so a gate runs the same tool on a laptop (arm64 or amd64) and in the private CI. Nothing builds this file:
# tools/lint/lint.sh reads the `FROM <ref> AS <stage>` lines, and tools/lint/check-pins.rb holds ci.yml's
# `lint-<gate>` jobs to the same refs. It is a Dockerfile so that Dependabot's and Renovate's docker managers bump
# a tag and its digest here like any other dependency; a bump changes the ci.yml job images in the same commit
# (check-pins.rb fails until it does).
#
# A new tool: a stage here (or, for a tool whose image has an ENTRYPOINT, a pin in tools.txt), a `case` arm in lint.sh's `gate_stage` and `gate_run`, a `lint-<gate>` job in ci.yml.
# The image gates need a built image and a Docker daemon, so they are not lint.sh's: tools/lint/image.sh
# reads its stages here the same way, and `make image-ci` runs them in the private CI's `image` job.

# cargo, rustfmt, clippy and rustdoc, and the Rust gates' release binaries (tools.txt): the toolchain the service is built with (Dockerfile pins the same digest).
FROM rust:1.98.0-trixie@sha256:620dbcd124499c59e2406d3741574b5c5838cf9eb9656f0c3a03948f79b02959 AS rust

# The `msrv` gate: Cargo.toml's rust-version, exactly (a bump of rust-version moves this pin in the same commit).
FROM rust:1.88.0-trixie@sha256:f2a17efbe58b00470be6e73dbea79705a789ee20ee8de2dcdaf73c0c6091f1db AS msrv

# the forge's workflow lint and check-pins.rb (Ruby stdlib).
FROM ruby:3.3.12-alpine@sha256:ac11166d133f476b0c3b4ae6fa6c6301cfc9a6f4e03c0761c482f00808c96cad AS ruby

# The Python gates of tools/ (Python stdlib): public, commits.
FROM python:3.14.8-alpine@sha256:f6a589d43c42b9e7f7dc67a12d37132491f362859a5d750607710cc56da3bc72 AS python

# hadolint: Dockerfile, tools/*/Dockerfile and this file (.hadolint.yaml).
FROM hadolint/hadolint:v2.14.0-alpine@sha256:7aba693c1442eb31c0b015c129697cb3b6cb7da589d85c7562f9deb435a6657c AS hadolint

# The image gates of tools/lint/image.sh. `build-check`: the Docker CLI with its buildx plugin, because `--check` needs
# buildx 0.15 and Debian trixie's docker-buildx (what the private CI image job's container gets) is older (0132b).
FROM docker:29.5.2-cli@sha256:9ba8e32bfc35a2c7ae2feb1e3241b2778ae21dee80f4dcd31d04e1cfdea86ea2 AS docker-cli

# Trivy (vulnerabilities, secrets, the image config), Dockle (the CIS Docker
# benchmark), syft (the SBOM).
FROM aquasec/trivy:0.76.1@sha256:790fd66d1697395d8693d1acaa1ea702406a20f109b6fc7bac8db623eca6c06c AS trivy
FROM goodwithtech/dockle:v0.4.15@sha256:eade932f793742de0aa8755406c7677cd7696f8675b6180926f7eeffa7abe6b9 AS dockle
FROM anchore/syft:v1.42.0@sha256:19ed45c8f4bf846390b072966a2fec52279d09f9e47650830ffabec7eebaf0fe AS syft

# The chart's gates: chart-testing's image carries ct, Helm 3, yamllint and yamale, and its ENTRYPOINT is
# empty, so a GitRiver job can run in it (GitRiver keeps an image's ENTRYPOINT and hands it the job's `sh -c`, which
# kubeconform's, kube-linter's, helm-unittest's and helm-docs' own images would answer with an error). Those four are
# release binaries pinned by sha256 in tools/lint/tools.txt and fetched into it (tools/lint/fetch-tool.sh).
FROM quay.io/helmpack/chart-testing:v3.15.0@sha256:54317f2d93730ea9ca1236e3d79cd95d74110b8d733b6ab124e5829209b5f4e4 AS chart-testing

# The `markdownlint` gate: markdownlint-cli2, installed by `npm ci` from tools/lint/markdownlint/'s
# lockfile, so every package of its tree is pinned by version and integrity hash. Its own image has the tool as its
# ENTRYPOINT, which GitRiver keeps; node's entrypoint runs a command it is given.
FROM node:24.22.1-alpine3.24@sha256:06d7adf3e0ef61a13f1d28682726366f97a15aa1888c934d3ecec86c33918b53 AS node

# `make release-notes` (tools/release/release-notes.sh): git-cliff drafts the next CHANGELOG.md section from the
# commits' release-note blocks (cliff.toml). Not a gate: a maintainer runs it at a release, and the tests of
# tools/release/ run it on a fixture history. Its ENTRYPOINT is git-cliff, which `docker run` hands the arguments.
FROM orhunp/git-cliff:2.10.1@sha256:6ba0d1fcb051bd7b154cfb19c4b2b3bfa2c22c475f5285fc30606777b6573119 AS git-cliff
