#!/bin/sh
# shellcheck disable=SC2015 # `test && ok … || fail …` is an if-then-else here: ok() only echoes and cannot fail.
# Installs charts/docx-to-pdf into a throwaway kind cluster and converts a document through the release's Service,
# from a pod inside the cluster; then checks that the chart's NetworkPolicy is enforced, the pod reaching the cluster
# DNS and nothing else. Exits non-zero on any failure; the cluster is always deleted.
#
# Usage: tools/chart/kind-test.sh [IMAGE]   (default docx-to-pdf:dev, the host's architecture: `make image`)
#
# kind, kubectl and helm are the release binaries pinned in tools/lint/tools.txt (tools/lint/fetch-tool.sh), so the run is
# the same on a laptop and in the private CI's image job (`make image-ci`), which runs it on Linux with the host's Docker
# socket and network. Anywhere else (macOS) the script runs itself again in the pinned `docker-cli` stage of
# tools/lint/images.Dockerfile, with the Docker socket and the Docker host's network, where kind's API server listens.
# It never runs an image of another architecture: kind's node runs the Docker host's.
#
# Environment:
#   KIND=<path>        a kind binary instead of the pinned one; KUBECTL=, HELM= the same for kubectl and helm
#   CLUSTER=<name>     the kind cluster (default docx-to-pdf-chart-<pid>); its node container is named <name>-control-plane
#   DOC=<path>         the document to convert (default tests/corpus/documents/text/text-1p.docx)
#   READY_TIMEOUT=<s>  seconds to wait for the rollout (default 300)
#   VALUES=<file>      extra values passed to helm install (optional)
#
# Never with an amd64 image on Apple Silicon: kind's node runs the host's architecture.
set -u
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
IMAGE=${1:-docx-to-pdf:dev}

if [ "$(uname -s)" != Linux ]; then
	ref=$(awk '$1 == "FROM" && $3 == "AS" && $4 == "docker-cli" { print $2 }' "$ROOT/tools/lint/images.Dockerfile")
	docker image inspect "$IMAGE" > /dev/null 2>&1 || {
		echo "no image $IMAGE (make image)"
		exit 2
	}
	exec docker run --rm --init --name "docx-to-pdf-chart-kind-$$" --network host \
		-v /var/run/docker.sock:/var/run/docker.sock -v "$ROOT:$ROOT" -w "$ROOT" \
		-e CLUSTER -e DOC -e READY_TIMEOUT -e VALUES -e KIND_NODE "$ref" sh tools/chart/kind-test.sh "$IMAGE"
fi

tool() {
	sh "$ROOT/tools/lint/fetch-tool.sh" "$1" || {
		echo "cannot fetch $1 (tools/lint/tools.txt)" >&2
		return 2
	}
}
[ -n "${KIND:-}" ] || KIND=$(tool kind) || exit 2
[ -n "${KUBECTL:-}" ] || KUBECTL=$(tool kubectl) || exit 2
[ -n "${HELM:-}" ] || HELM=$(tool helm) || exit 2
# The rest of this script calls kubectl and helm by name; each pinned binary sits in a directory of its own.
PATH="$(dirname "$KUBECTL"):$(dirname "$HELM"):$PATH"
CLUSTER=${CLUSTER:-docx-to-pdf-chart-$$}
DOC=${DOC:-$ROOT/tests/corpus/documents/text/text-1p.docx}
READY_TIMEOUT=${READY_TIMEOUT:-300}
VALUES=${VALUES:-}
# kind v0.30.0's node image (Kubernetes 1.34.0) and curl 8.16.0, by digest (multi-arch indexes).
KIND_NODE=${KIND_NODE:-kindest/node:v1.34.0@sha256:7416a61b42b1662ca6ca89f02028ac133a309a2a30ba309614e8ec94d976dc5a}
CURL_IMAGE=curlimages/curl:8.16.0@sha256:463eaf6072688fe96ac64fa623fe73e1dbe25d8ad6c34404a669ad3ce1f104b6
CHART=$ROOT/charts/docx-to-pdf
RELEASE=docx-to-pdf
NS=docx-to-pdf
OUT=$(mktemp -d)

command -v docker > /dev/null || {
	echo "the host lacks docker"
	exit 2
}
[ -f "$CHART/Chart.yaml" ] || {
	echo "FAIL    no chart at $CHART"
	exit 1
}
docker image inspect "$IMAGE" > /dev/null 2>&1 || {
	echo "no image $IMAGE (make image)"
	exit 2
}

# kind pulls its node image (about 1 GB) once, unretried: pull it first, with retries (tools/ci/pull.sh). The pods'
# images (curl, the chart's test) are the kubelet's to pull, and it retries on its own within the waits below.
sh "$ROOT/tools/ci/pull.sh" "$KIND_NODE" || {
	echo "FAIL    cannot pull $KIND_NODE"
	exit 1
}

FAILED=0
ok() { echo "ok      $*"; }
fail() {
	echo "FAIL    $*"
	FAILED=1
}

KUBECONFIG=$OUT/kubeconfig
export KUBECONFIG
# shellcheck disable=SC2329 # run by the trap below
cleanup() {
	"$KIND" delete cluster --name "$CLUSTER" > /dev/null 2>&1
	rm -rf "$OUT"
}
trap cleanup EXIT INT TERM

echo "cluster $CLUSTER ($KIND_NODE), image $IMAGE"
"$KIND" create cluster --name "$CLUSTER" --image "$KIND_NODE" --wait 120s > "$OUT/create.log" 2>&1 ||
	{
		echo "FAIL    kind create cluster"
		tail -5 "$OUT/create.log"
		exit 1
	}
"$KIND" load docker-image "$IMAGE" --name "$CLUSTER" > "$OUT/load.log" 2>&1 ||
	{
		echo "FAIL    kind load docker-image"
		tail -5 "$OUT/load.log"
		exit 1
	}

REPO=${IMAGE%:*}
TAG=${IMAGE##*:}
# The chart's defaults otherwise: pool 1, the computed requests and limits (2560 MiB), the PDB.
set -- --namespace "$NS" --create-namespace \
	--set image.repository="$REPO" --set image.tag="$TAG" --set image.pullPolicy=Never
[ -n "$VALUES" ] && set -- "$@" --values "$VALUES"
if helm install "$RELEASE" "$CHART" "$@" --wait --timeout "${READY_TIMEOUT}s" > "$OUT/install.log" 2>&1; then
	ok "helm install: the release is ready"
else
	fail "helm install: $(tail -1 "$OUT/install.log")"
	kubectl -n "$NS" describe pods | tail -30
	kubectl -n "$NS" logs -l app.kubernetes.io/instance="$RELEASE" --tail 30
	exit 1
fi

# The chart's own test (templates/test-convert.yaml), as `ct install` runs it: a pod converts an RTF document through
# the Service.
if helm test "$RELEASE" --namespace "$NS" --timeout 180s --logs > "$OUT/test.log" 2>&1 &&
	grep -q ': a PDF$' "$OUT/test.log"; then
	ok "helm test: $(grep ': a PDF$' "$OUT/test.log")"
else
	fail "helm test: $(tail -3 "$OUT/test.log" | tr '\n' ' ')"
	kubectl -n "$NS" logs "$RELEASE-test-convert" --tail 10 2> /dev/null
fi

# The pod runs under the security context the chart asks for.
POD=$(kubectl -n "$NS" get pods -l app.kubernetes.io/instance="$RELEASE" -o jsonpath='{.items[0].metadata.name}')
ID=$(kubectl -n "$NS" exec "$POD" -- sh -c 'id -u; id -g; touch /probe-root 2>/dev/null && echo rw || echo ro; touch /tmp/x && echo tmp-rw')
[ "$(echo "$ID" | tr '\n' ' ')" = "10001 10001 ro tmp-rw " ] && ok "uid 10001, read-only root, /tmp writable" ||
	fail "security context: $(echo "$ID" | tr '\n' ' ')"

# A client pod converts through the Service's DNS name: Service → endpoints → pod.
SVC=http://$RELEASE.$NS.svc.cluster.local:8080
kubectl -n "$NS" run client --image "$CURL_IMAGE" --restart Never --command -- sleep 600 > /dev/null
kubectl -n "$NS" wait --for condition=Ready pod/client --timeout 120s > /dev/null || fail "the client pod did not start"
kubectl -n "$NS" exec client -- curl -fsS "$SVC/readyz" > "$OUT/readyz" 2>&1 && ok "GET /readyz through the Service: $(cat "$OUT/readyz")" ||
	fail "GET /readyz through the Service: $(cat "$OUT/readyz")"
# shellcheck disable=SC2094 # $DOC is read twice (its name, its bytes) and written nowhere
STATUS=$(kubectl -n "$NS" exec -i client -- curl -sS -o /tmp/out.pdf -w '%{http_code} %{content_type}' \
	-F "document=@-;filename=$(basename "$DOC")" "$SVC/pdf" < "$DOC")
kubectl -n "$NS" exec client -- cat /tmp/out.pdf > "$OUT/out.pdf"
if [ "$STATUS" = "200 application/pdf" ] && [ "$(head -c 5 "$OUT/out.pdf")" = "%PDF-" ]; then
	ok "POST /pdf through the Service: $STATUS, $(wc -c < "$OUT/out.pdf" | tr -d ' ') bytes of PDF"
else
	fail "POST /pdf through the Service: $STATUS, starts $(head -c 16 "$OUT/out.pdf" | od -c | head -1)"
fi
kubectl -n "$NS" exec client -- curl -fsS "$SVC/metrics" 2> /dev/null | grep -q '^docx_to_pdf_conversions_total' &&
	ok "GET /metrics counts the conversion" || fail "GET /metrics has no docx_to_pdf_conversions_total"

# The chart's NetworkPolicy, on by default and enforced by kind's CNI, kindnet, which enforces
# NetworkPolicy since kind v0.24.0: the test above already reached the pod's port 8080 through it. Here a debug
# container in the service's pod, in its network namespace and under its policy, resolves a name through the cluster
# DNS but reaches nothing else, while the client pod, which no policy selects, reaches the same addresses. kindnet
# polices what a pod sends through the node, not to the node itself, so the targets are other pods and Services: on a
# one-node kind cluster the API server is the node's own address, which the pod reaches whatever the policy.
if kubectl -n "$NS" get networkpolicy "$RELEASE" > /dev/null 2>&1; then
	ok "the chart's NetworkPolicy $RELEASE is installed"
	kubectl -n "$NS" debug "$POD" --image "$CURL_IMAGE" --container netcheck --profile restricted \
		-- sh -c 'trap "exit 0" TERM; sleep 600 & wait' \
		> "$OUT/debug.log" 2>&1 || fail "kubectl debug: $(tail -1 "$OUT/debug.log")"
	for _ in $(seq 60); do
		[ -n "$(kubectl -n "$NS" get pod "$POD" \
			-o jsonpath='{.status.ephemeralContainerStatuses[?(@.name=="netcheck")].state.running}')" ] && break
		sleep 2
	done
	kubectl -n "$NS" exec "$POD" -c netcheck -- nslookup kubernetes.default.svc.cluster.local > "$OUT/dns" 2>&1 &&
		ok "the pod resolves kubernetes.default.svc.cluster.local through the cluster DNS" ||
		fail "the pod cannot resolve a name: $(tail -2 "$OUT/dns" | tr '\n' ' ')"
	# The DNS Service and a DNS pod, each on a port other than 53 (CoreDNS's metrics, 9153), and the release's own
	# Service, which the converter has no reason to call.
	DNSSVC=http://$(kubectl -n kube-system get service kube-dns -o jsonpath='{.spec.clusterIP}'):9153/metrics
	DNSPOD=http://$(kubectl -n kube-system get pods -l k8s-app=kube-dns -o jsonpath='{.items[0].status.podIP}'):9153/metrics
	for url in "$DNSSVC" "$DNSPOD" "$SVC/readyz"; do
		if ! kubectl -n "$NS" exec client -- curl -sSk -o /dev/null --max-time 5 "$url" > "$OUT/reach" 2>&1; then
			fail "the client pod cannot reach $url either, so the policy shows nothing: $(cat "$OUT/reach")"
		elif kubectl -n "$NS" exec "$POD" -c netcheck -- curl -sSk -o /dev/null --max-time 5 "$url" > "$OUT/reach" 2>&1; then
			fail "the pod reached $url through its NetworkPolicy"
		else
			ok "the pod cannot reach $url, the client pod can ($(head -1 "$OUT/reach"))"
		fi
	done
else
	fail "no NetworkPolicy $RELEASE in $NS"
fi

# Graceful shutdown: helm uninstall deletes the pod; it must go within its grace period, not by SIGKILL at its end.
GRACE=$(kubectl -n "$NS" get pod "$POD" -o jsonpath='{.spec.terminationGracePeriodSeconds}')
START=$(date +%s)
helm uninstall "$RELEASE" --namespace "$NS" --wait --timeout 300s > "$OUT/uninstall.log" 2>&1 ||
	fail "helm uninstall: $(tail -1 "$OUT/uninstall.log")"
kubectl -n "$NS" wait --for delete "pod/$POD" --timeout 300s > /dev/null 2>&1
TOOK=$(($(date +%s) - START))
[ "$TOOK" -lt "$GRACE" ] && ok "the pod stopped in ${TOOK}s (grace ${GRACE}s)" || fail "the pod took ${TOOK}s (grace ${GRACE}s)"

[ "$FAILED" = 0 ] && echo "PASS" || echo "FAILED"
exit "$FAILED"
