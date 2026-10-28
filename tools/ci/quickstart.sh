#!/bin/sh
# The README's quick start against an image, as a reader types it. Reads the `docker run` and the `curl`
# lines of README.md's "Quick start" (its ```sh blocks; not a copy of them), runs the `docker run` with the registry
# reference swapped for IMAGE, waits until the service is ready, runs the `curl` from a directory holding the
# document it uploads, and expects `200` and a PDF. Exits non-zero on any failure; the container is always removed.
#
#   sh tools/ci/quickstart.sh IMAGE [README]     (`make image-quickstart`; README defaults to README.md)
#
# What is added to the README's lines, and why: `--detach --name <name>` after `docker run` (the README runs it in a
# terminal of its own; this needs the name to remove it), and `--silent --show-error --write-out %{http_code}` at the
# end of the `curl` (the status to check). Every flag the README gives is kept, so a port or a flag the image does not
# serve fails here.
#
# Where the curl runs. The `docker run` publishes on the Docker host. On Linux (the CI image job's container, on the
# host's network) that host is this one, so the curl runs here. Anywhere else (macOS) the Docker host is a VM whose
# loopback is not this machine's, and the README's port may be taken here: the curl runs in the pinned curl image on
# the Docker host's network, the directory bind-mounted at its own path (under target/, which the VM sees).
#
# Environment: DOC=<path> the document uploaded under the README's file name (default
# tests/corpus/documents/text/text-1p.docx); READY_TIMEOUT=<s> (default 120).
set -u
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
IMAGE=${1:?usage: sh tools/ci/quickstart.sh IMAGE [README]}
README=${2:-$ROOT/README.md}
DOC=${DOC:-$ROOT/tests/corpus/documents/text/text-1p.docx}
READY_TIMEOUT=${READY_TIMEOUT:-120}
# The registry reference the README names, whatever its tag (tests/readme.rs checks the tag is this version's).
REGISTRY=quay.io/oxidestack/docx-to-pdf:
# curl 8.16.0 by digest (a multi-arch index), the one tools/chart/kind-test.sh pins.
CURL_IMAGE=curlimages/curl:8.16.0@sha256:463eaf6072688fe96ac64fa623fe73e1dbe25d8ad6c34404a669ad3ce1f104b6
NAME=docx-to-pdf-quickstart-$$

fail() {
	echo "FAIL    $*"
	exit 1
}

[ -f "$README" ] || fail "no $README"
docker image inspect "$IMAGE" > /dev/null 2>&1 || {
	echo "no image $IMAGE (make image)"
	exit 2
}

# The lines of the Quick start section's ```sh blocks that start with $1.
quick_start() {
	awk -v program="$1" '
		/^## / { in_section = ($0 == "## Quick start") }
		in_section && /^```/ { if (fence) fence = 0; else fence = ($0 == "```sh"); next }
		in_section && fence && $1 == program { print }
	' "$README"
}
RUN=$(quick_start docker)
CURL=$(quick_start curl)
[ "$(printf '%s\n' "$RUN" | grep -c .)" = 1 ] || fail "$README: the quick start has not exactly one docker line: $RUN"
[ "$(printf '%s\n' "$CURL" | grep -c .)" = 1 ] || fail "$README: the quick start has not exactly one curl line: $CURL"
echo "README  $RUN"
echo "README  $CURL"

# The docker run: its words as the shell splits them, the registry reference swapped for IMAGE.
eval "set -- $RUN"
[ "$1 $2" = "docker run" ] || fail "not a docker run: $RUN"
shift 2
run_args=""
swapped=0
for word in "$@"; do
	case "$word" in
	"$REGISTRY"*)
		word=$IMAGE
		swapped=1
		;;
	esac
	run_args="$run_args '$(printf '%s' "$word" | sed "s/'/'\\\\''/g")'"
done
[ "$swapped" = 1 ] || fail "$README: the docker run names no $REGISTRY<tag>"

# The curl: the file it uploads (-F field=@file), the file it writes (-o), the URL (its origin for /readyz).
eval "set -- $CURL"
upload="" output="" url=""
prev=""
for word in "$@"; do
	case "$prev:$word" in
	-F:*=@*) upload=${word#*=@} ;;
	-o:*) output=$word ;;
	*:http://* | *:https://*) url=$word ;;
	esac
	prev=$word
done
[ -n "$upload" ] && [ -n "$output" ] && [ -n "$url" ] || fail "$README: the curl has no -F field=@file, -o file or URL"
origin=$(printf '%s' "$url" | sed -E 's#^(https?://[^/]+).*#\1#')

WORK=$ROOT/target/quickstart.$$
cleanup() {
	docker rm -f -v "$NAME" > /dev/null 2>&1
	rm -rf "$WORK"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
mkdir -p "$WORK"
cp "$DOC" "$WORK/$upload"

if [ "$(uname -s)" = Linux ]; then
	curl_here() { (cd "$WORK" && curl "$@"); }
else
	curl_here() {
		docker run --rm --network host -v "$WORK:$WORK" -w "$WORK" --user "$(id -u):$(id -g)" "$CURL_IMAGE" "$@"
	}
fi

echo "run     docker run --detach --name $NAME$run_args"
eval "docker run --detach --name $NAME$run_args" > "$WORK/run.log" 2>&1 || {
	cat "$WORK/run.log"
	fail "the quick start's docker run did not start against $IMAGE"
}

start=$(date +%s)
until curl_here -fsS -o /dev/null "$origin/readyz" > /dev/null 2>&1; do
	if [ "$(docker inspect -f '{{.State.Running}}' "$NAME" 2> /dev/null)" != true ]; then
		docker logs --tail 20 "$NAME" 2>&1
		fail "the container stopped before $origin/readyz answered"
	fi
	[ $(($(date +%s) - start)) -lt "$READY_TIMEOUT" ] || {
		docker logs --tail 20 "$NAME" 2>&1
		fail "$origin/readyz not ready within ${READY_TIMEOUT}s (the README's port or flags?)"
	}
	sleep 1
done
echo "ok      $origin/readyz after $(($(date +%s) - start))s"

eval "set -- $CURL"
shift
status=$(curl_here "$@" --silent --show-error --write-out '%{http_code}') || fail "the quick start's curl failed: $status"
[ "$status" = 200 ] || fail "the quick start's curl answered $status: $(head -c 300 "$WORK/$output" 2> /dev/null)"
[ "$(head -c 5 "$WORK/$output")" = "%PDF-" ] || fail "the quick start's $output is not a PDF"
echo "ok      the quick start's curl: 200, $output a PDF of $(wc -c < "$WORK/$output" | tr -d ' ') bytes"
