#!/bin/sh
# Replays every moalhaddar/docx-to-pdf scenario (tests/scenarios/) against the service's image, run as a deployment
# runs it, so the scenarios check what ships and not only the code; with SUITE=corpus, runs the corpus instead
# (tests/corpus.rs).
# Usage: tests/oracle/image-scenarios.sh [IMAGE] [SUITE]   (default docx-to-pdf:dev and scenarios; `make
# image-scenarios`, `make image-corpus`). Exits with the suite's status.
#
# Environment:
#   NAME=docx-to-pdf-image-scenarios   the container's name prefix (it is removed, with its volumes, at exit)
#   READY_TIMEOUT=<s>                  seconds to wait for /readyz (default 300: an emulated amd64 starts slowly)
#   DOCX_TO_PDF_SCENARIOS_STRICT=1     passed through: ignore `pending`, every scenario must pass
#
# The container runs under the security context a Kubernetes deployment gives it, as image/test.sh does: uid 10001
# with no passwd entry, a read-only root filesystem, only /tmp writable (on disk, the default), no capabilities.
set -u
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
IMAGE=${1:-docx-to-pdf:dev}
SUITE=${2:-scenarios}
NAME=${NAME:-docx-to-pdf-image-scenarios}
READY_TIMEOUT=${READY_TIMEOUT:-300}

ARCH=$(docker image inspect -f '{{.Architecture}}' "$IMAGE") || {
	echo "no image $IMAGE"
	exit 2
}
C="$NAME-$$"
# shellcheck disable=SC2329 # run by the trap below
cleanup() { docker rm -f -v "$C" > /dev/null 2>&1; }
trap cleanup EXIT INT TERM

docker run -d --platform "linux/$ARCH" --name "$C" --user 10001:10001 --read-only --cap-drop ALL \
	--security-opt no-new-privileges --mount type=volume,dst=/tmp -p 127.0.0.1::8080 "$IMAGE" > /dev/null ||
	{
		echo "the container did not start"
		exit 2
	}
PORT=$(docker port "$C" 8080/tcp | head -1 | sed 's/.*://')
echo "image $IMAGE (linux/$ARCH) as $C on 127.0.0.1:$PORT"

since=$(date +%s)
until [ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/readyz")" = 200 ]; do
	if [ $(($(date +%s) - since)) -gt "$READY_TIMEOUT" ]; then
		echo "/readyz not 200 after ${READY_TIMEOUT}s; the container's log:"
		docker logs --tail 40 "$C" 2>&1
		exit 2
	fi
	sleep 0.2
done
echo "ready after $(($(date +%s) - since))s"

cd "$ROOT" || exit 2
case "$SUITE" in
scenarios) DOCX_TO_PDF_SCENARIOS_TARGET="http://127.0.0.1:$PORT" cargo test --test oracle scenarios -- --exact --nocapture ;;
corpus) DOCX_TO_PDF_CORPUS_TARGET="http://127.0.0.1:$PORT" cargo test --test corpus corpus -- --exact --nocapture ;;
*)
	echo "unknown suite $SUITE (scenarios, corpus)"
	exit 2
	;;
esac
status=$?
[ "$status" = 0 ] || {
	echo "the container's log (last 40 lines):"
	docker logs --tail 40 "$C" 2>&1
}
exit "$status"
