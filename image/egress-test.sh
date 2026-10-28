#!/bin/sh
# shellcheck disable=SC2015 # `test && ok … || fail …` is an if-then-else here: ok() only echoes and cannot fail.
# A conversion opens no outbound connection and links no local file, whatever external reference the document
# carries: a fetch from any address the document names, internal ones included, is server-side request forgery from
# inside the cluster, and a host that never answers holds the conversion 5-10 s per picture.
# Usage: image/egress-test.sh [IMAGE]   (default docx-to-pdf:dev; `make image-test` runs it after image/test.sh).
# Exits non-zero on any failure.
#
# The service and a listener (image/egress/listener.py) share two internal Docker networks and nothing else: a
# private one (the listener at 10.231.N.10, its port 81 holds a connection without answering) and one standing in
# for the internet (198.18.N.10, its port 80 serves content); N is picked free at the start, so two runs on one
# Docker host do not collide. Every document image/egress/documents.py writes, one
# per kind of reference and target, is converted by a client on the private network; the listener must log no
# connection. A CA bundle is mounted into the service, so the check holds where a deployment adds one (the image has
# none, and without one LibreOffice's HTTP client never starts).
#
# Environment: NAME (default docx-to-pdf-egress-test), OUT (default target/egress-test; shared with the Docker VM).
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd)
IMAGE=${1:-docx-to-pdf:dev}
NAME=${NAME:-docx-to-pdf-egress-test}
OUT=${OUT:-$ROOT/target/egress-test}
# python:3.14.8-alpine, by digest (an index: the host's platform is picked).
PYTHON=python@sha256:f6a589d43c42b9e7f7dc67a12d37132491f362859a5d750607710cc56da3bc72
READY_TIMEOUT=${READY_TIMEOUT:-300}

FAILED=0
ok() { echo "ok      $*"; }
fail() {
	echo "FAIL    $*"
	FAILED=1
}

for tool in python3 pdfimages; do
	command -v "$tool" > /dev/null || {
		echo "the host lacks $tool (python3, poppler-utils)"
		exit 2
	}
done
rm -rf "$OUT" && mkdir -p "$OUT/docs" "$OUT/pdf" && OUT=$(cd "$OUT" && pwd)
chmod 777 "$OUT/pdf"
: > "$OUT/ca-bundle.crt"
# The listener's `docker run` would pull PYTHON once, unretried: pull it first, with retries (tools/ci/pull.sh).
sh "$ROOT/tools/ci/pull.sh" "$PYTHON" || {
	echo "cannot pull $PYTHON"
	exit 2
}

S=$$
# shellcheck disable=SC2329 # run by the trap below
cleanup() {
	for c in $(docker ps -aq --filter "name=^$NAME-.*-$S\$"); do docker rm -f -v "$c" > /dev/null 2>&1; done
	docker network rm "$NAME-private-$S" "$NAME-public-$S" > /dev/null 2>&1
}
trap cleanup EXIT INT TERM

n=$((S % 250))
tries=0
until docker network create --internal --subnet "10.231.$n.0/24" "$NAME-private-$S" > /dev/null 2>&1 &&
	{ docker network create --internal --subnet "198.18.$n.0/24" "$NAME-public-$S" > /dev/null 2>&1 ||
		{
			docker network rm "$NAME-private-$S" > /dev/null
			false
		}; }; do
	tries=$((tries + 1))
	[ $tries -lt 10 ] || {
		echo "no free 10.231.N.0/24 and 198.18.N.0/24"
		exit 2
	}
	n=$(((n + 1) % 250))
done
PRIVATE=10.231.$n
PUBLIC=198.18.$n
python3 "$ROOT/image/egress/documents.py" "$OUT/docs" "$PUBLIC.10" "$PRIVATE.10" > /dev/null ||
	{
		echo "documents.py failed"
		exit 2
	}
docker run -d --name "$NAME-listener-$S" --network "$NAME-private-$S" --ip "$PRIVATE.10" \
	-v "$ROOT/image/egress:/egress:ro" "$PYTHON" python -u /egress/listener.py 80 81 443 21 445 > /dev/null &&
	docker network connect --ip "$PUBLIC.10" "$NAME-public-$S" "$NAME-listener-$S" ||
	{
		echo "cannot start the listener"
		exit 2
	}
# A started container is not yet a bound socket: the listener's Python starts while the next containers do, and a
# probe sent before it binds is refused (seen on a 2-vCPU runner: bound 1-2 s after the probe). Its own line says when.
tries=0
until docker logs "$NAME-listener-$S" 2>&1 | grep -q '"event": "listening"'; do
	tries=$((tries + 1))
	[ $tries -lt 60 ] || {
		echo "the listener did not start listening in 30 s"
		docker logs "$NAME-listener-$S" 2>&1 | tail -20
		exit 2
	}
	sleep 0.5
done
docker run -d --name "$NAME-service-$S" --network "$NAME-private-$S" --ip "$PRIVATE.20" --user 10001:10001 --read-only \
	--cap-drop ALL --security-opt no-new-privileges --tmpfs /tmp:rw,mode=1777,size=1g \
	-v "$OUT/ca-bundle.crt:/etc/ssl/certs/ca-certificates.crt:ro" \
	-e DOCX_TO_PDF_POOL_SIZE=1 -e DOCX_TO_PDF_CONVERSION_TIMEOUT=120s "$IMAGE" > /dev/null &&
	docker network connect --ip "$PUBLIC.20" "$NAME-public-$S" "$NAME-service-$S" ||
	{
		echo "cannot start the service"
		exit 2
	}

# client <network> <script>: Python on one of the two networks, the documents and the output mounted.
client() {
	net=$1
	shift
	docker run --rm --name "$NAME-client-$S" --network "$NAME-$net-$S" -v "$ROOT/image/egress:/egress:ro" \
		-v "$OUT/docs:/docs:ro" -v "$OUT/pdf:/out" "$PYTHON" python -u "$@"
}

# The listener answers on both networks: a connection the service opened would reach it and be logged.
reach() { client "$1" -c "import urllib.request; urllib.request.urlopen('http://$2/probe', timeout=5)"; }
reach private "$PRIVATE.10" && reach public "$PUBLIC.10" && ok "the listener answers on $PRIVATE.10 and $PUBLIC.10" || {
	fail "the listener is unreachable"
	docker logs "$NAME-listener-$S" 2>&1 | tail -20
}
# editorconfig-checker-disable: the Python below is indented with spaces, as Python is
client private -c "
import time, urllib.request
for _ in range($READY_TIMEOUT):
    try:
        urllib.request.urlopen('http://$PRIVATE.20:8080/readyz', timeout=2); break
    except Exception:
        time.sleep(1)
else:
    raise SystemExit(1)
" || {
	fail "the service did not become ready"
	docker logs "$NAME-service-$S" 2>&1 | tail -20
	exit 1
}
# editorconfig-checker-enable
before=$(docker logs "$NAME-listener-$S" 2>&1 | grep -c '"connect"')

client private /egress/client.py "http://$PRIVATE.20:8080/pdf" > /dev/null
docker logs "$NAME-listener-$S" > "$OUT/listener.jsonl" 2>&1

# editorconfig-checker-disable: the Python below is indented with spaces, as Python is
python3 - "$OUT" "$before" << 'PY' || FAILED=1
import json, subprocess, sys
out, before = sys.argv[1], int(sys.argv[2])
results = [json.loads(l) for l in open(f"{out}/pdf/results.jsonl")]
connections = [json.loads(l) for l in open(f"{out}/listener.jsonl") if '"connect"' in l][before:]
failed = 0
def check(cond, good, bad):
    global failed
    print(("ok      " if cond else "FAIL    ") + (good if cond else bad))
    failed |= not cond
def images(doc):
    listing = subprocess.run(["pdfimages", "-list", f"{out}/pdf/{doc}.pdf"], capture_output=True, text=True).stdout
    return len(listing.splitlines()) - 2
bad = [r["doc"] for r in results if r["status"] != 200]
check(not bad, f"all {len(results)} documents convert", f"not converted: {bad}")
check(not connections, f"no outbound connection from {len(results)} documents with external references",
      "outbound connections: " + "; ".join(f"{c['local']} {c['first'][:60]!r}" for c in connections))
slow = [f"{r['doc']} {r['seconds']} s" for r in results if r["seconds"] > 5]
check(not slow, "no conversion waits on a host (10 links to a host that never answers: "
      + str(next(r["seconds"] for r in results if r["doc"].startswith("d-linked-image-x10--int"))) + " s)",
      f"conversions waited: {slow}")
if not bad:
    check(images("d-linked-image--file.docx") == 0, "a picture linked to a local file is not embedded",
          "a picture linked to file:///usr/lib/libreoffice/program/shell/donate2.png was embedded")
    check(images("d-embed-and-link--pub.docx") == 1, "a picture embedded and linked keeps its embedded copy",
          "the embedded copy of a linked picture is missing")
    # RTF's INCLUDEPICTURE reads a local file past every LibreOffice setting. Accepted: the instance can read only the
    # image's own files and /tmp, where uploads live under random names, and nothing secret there is an image.
    print(f"note    RTF INCLUDEPICTURE of a local file: {images('r-includepicture--file.rtf')} image(s) in the PDF")
sys.exit(failed)
PY
# editorconfig-checker-enable

[ $FAILED = 0 ] && echo "egress-test passed" || echo "egress-test FAILED"
exit $FAILED
