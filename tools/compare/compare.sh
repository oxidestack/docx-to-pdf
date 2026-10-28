#!/bin/sh
# `make compare [QUICK=1]`. Builds oxidestack/docx-to-pdf's image for this host's architecture from the checkout
# (every run: the build cache makes it a no-op when nothing changed, and the image measured is the checkout's), the
# bench's tool image when it is missing and the runner, then runs tools/compare/compare.py in the runner with the host's
# Docker socket (examples/compare/compose.yaml, service `compare`). Needs only Docker with Compose v2.
#
#   tools/compare/compare.sh [--quick] [compare.py options]       IMAGE=<ref> tests that image instead of building one
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
COMPARE_ROOT=$ROOT
COMPARE_IMAGE=${IMAGE:-docx-to-pdf:compare}
# The commit the image built here is labelled with (org.opencontainers.image.revision): `make compare` passes the
# Makefile's IMAGE_REVISION; run directly, the same rule (HEAD, `-dirty` when tracked files differ, nothing outside
# git). An image given with IMAGE= keeps the label it was built with: the runner records that one.
if [ -z "${IMAGE_REVISION+set}" ]; then
	IMAGE_REVISION=$(git -C "$ROOT" rev-parse HEAD 2> /dev/null || true)
	if [ -n "$IMAGE_REVISION" ] && [ -n "$(git -C "$ROOT" status --porcelain --untracked-files=no)" ]; then
		IMAGE_REVISION=$IMAGE_REVISION-dirty
	fi
fi
COMPARE_REVISION=$IMAGE_REVISION
export COMPARE_ROOT COMPARE_IMAGE COMPARE_REVISION
compose() { docker compose -f "$ROOT/examples/compare/compose.yaml" --profile compare "$@"; }

if [ -z "${IMAGE:-}" ]; then
	compose build docx-to-pdf
elif ! docker image inspect "$COMPARE_IMAGE" > /dev/null 2>&1; then
	echo "compare: no image $IMAGE" >&2
	exit 1
fi
docker image inspect docx-to-pdf-bench-tools:dev > /dev/null 2>&1 || compose build bench-tools
compose build compare
# What the run records beside its results: the command as given and the checkout's commit (the runner has no git), and
# the machine this script runs on (tools/bench/provenance.py, when there is a python3): on a Mac the runner sits in the
# Docker VM, which hides the chip.
COMPARE_COMMIT=$(git -C "$ROOT" describe --always --dirty --abbrev=12 2> /dev/null || echo unknown)
COMPARE_MACHINE=$(python3 "$ROOT/tools/bench/provenance.py" machine 2> /dev/null || echo null)
exec docker compose -f "$ROOT/examples/compare/compose.yaml" --profile compare run --rm --name docx-to-pdf-compare-runner \
	-e COMPARE_COMMIT="$COMPARE_COMMIT" -e COMPARE_MACHINE="$COMPARE_MACHINE" -e COMPARE_COMMAND="tools/compare/compare.sh $*" \
	compare "$@"
