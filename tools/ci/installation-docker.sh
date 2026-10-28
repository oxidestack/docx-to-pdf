#!/bin/sh
# The installation guide's Docker and helm tests against an image — the `#[ignore]`d tests of
# tests/installation.rs: `docker compose up` of examples/compose/ and its telemetry variant convert a document, and
# `helm template` renders the page's values. Checks first that the tools they call are there, so a missing one reads
# as that and not as a test failure.
#
#   sh tools/ci/installation-docker.sh IMAGE     (`make image-installation`)
#
# Needs Docker Compose 2.24 or later (the tests' override file uses `!override`), curl and helm. On Linux (the CI
# image job) helm is the release pinned in tools/lint/tools.txt (tools/lint/fetch-tool.sh); elsewhere, the one on PATH.
# The tests take down each compose project they start (`down --volumes`); this checks none is left.
set -u
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
IMAGE=${1:?usage: sh tools/ci/installation-docker.sh IMAGE}
cd "$ROOT" || exit 2

docker image inspect "$IMAGE" > /dev/null 2>&1 || {
	echo "no image $IMAGE (make image)"
	exit 2
}
command -v curl > /dev/null || {
	echo "no curl"
	exit 2
}
# "Docker Compose version v2.26.1", "2.26.1-4" (Debian), "v5.6.0".
compose=$(docker compose version --short 2> /dev/null) || {
	echo "no docker compose plugin"
	exit 2
}
major=$(printf '%s' "${compose#v}" | cut -d. -f1)
minor=$(printf '%s' "${compose#v}" | cut -d. -f2)
[ "$major" -gt 2 ] 2> /dev/null || { [ "$major" = 2 ] && [ "$minor" -ge 24 ] 2> /dev/null; } || {
	echo "docker compose $compose: the tests need 2.24 or later"
	exit 2
}
if [ "$(uname -s)" = Linux ]; then
	helm=$(sh tools/lint/fetch-tool.sh helm) || {
		echo "cannot fetch helm (tools/lint/tools.txt)"
		exit 2
	}
	PATH="$(dirname "$helm"):$PATH"
	export PATH
fi
command -v helm > /dev/null || {
	echo "no helm"
	exit 2
}
echo "docker compose $compose, $(helm version --short 2> /dev/null), $(curl --version | head -1 | cut -d' ' -f1-2)"

# The compose files' other images (the collector, pinned by digest; the service's own is replaced by IMAGE), pulled
# with retries: `docker compose up` would pull each once, unretried.
# shellcheck disable=SC2046 # a list of image references
sh tools/ci/pull.sh $(sed -n 's/^[[:space:]]*image:[[:space:]]*\([^[:space:]]*@sha256:[0-9a-f]*\).*/\1/p' \
	examples/compose/compose.yaml examples/compose/*/compose.yaml | sort -u) || exit 1

DOCX_TO_PDF_IMAGE=$IMAGE cargo test --test installation -- --ignored
code=$?

left=$(docker ps -a --filter label=com.docker.compose.project --format '{{.Label "com.docker.compose.project"}}' |
	grep '^docx-to-pdf-installation-' | sort -u)
[ -z "$left" ] || {
	echo "FAIL    compose projects left: $left"
	exit 1
}
exit "$code"
