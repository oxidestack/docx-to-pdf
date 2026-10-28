#!/bin/sh
# shellcheck disable=SC2015 # `test && ok … || fail …` is an if-then-else here: ok() only echoes and cannot fail.
# Installs charts/docx-to-pdf into a throwaway kind cluster and converts a document through the release's Service,
# from a pod inside the cluster; then checks that the chart's NetworkPolicy is enforced, the pod reaching the cluster
# DNS and nothing else. Exits non-zero on any failure; the cluster is always deleted.
#
# The cluster's node is a container on the Docker host, beside whatever runs this script, so it outlives a script
# that is killed: a cancelled CI job's containers are removed under it, and no trap of its runs. Run in a container
# (CI, or the macOS re-run below), the script therefore starts a reaper beside the node, a container that deletes the
# cluster once this script's container is gone. A cluster left behind is not harmless: each kind node holds about 40
# of the host's inotify instances (fs.inotify.max_user_instances, 128 by default), and with two others running a new
# cluster's kube-proxy exits with "too many open files", which leaves its Services unreachable and their names
# unresolved.
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
#   CLUSTER=<name>     the kind cluster (default docx-to-pdf-chart-<8 random hex digits>: a pid is no name, since a
#                      fresh CI container's pids repeat from job to job); its node container is <name>-control-plane
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
		-e CLUSTER -e DOC -e READY_TIMEOUT -e VALUES -e KIND_NODE -e CLI_IMAGE="$ref" "$ref" sh tools/chart/kind-test.sh "$IMAGE"
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
CLUSTER=${CLUSTER:-docx-to-pdf-chart-$(od -An -N4 -tx1 /dev/urandom | tr -d ' \n')}
DOC=${DOC:-$ROOT/tests/corpus/documents/text/text-1p.docx}
READY_TIMEOUT=${READY_TIMEOUT:-300}
VALUES=${VALUES:-}
# kind v0.30.0's node image (Kubernetes 1.34.0) and curl 8.16.0, by digest (multi-arch indexes).
KIND_NODE=${KIND_NODE:-kindest/node:v1.34.0@sha256:7416a61b42b1662ca6ca89f02028ac133a309a2a30ba309614e8ec94d976dc5a}
CURL_IMAGE=curlimages/curl:8.16.0@sha256:463eaf6072688fe96ac64fa623fe73e1dbe25d8ad6c34404a669ad3ce1f104b6
# The reaper's image: the Docker CLI, the pinned `docker-cli` stage of tools/lint/images.Dockerfile.
CLI_IMAGE=${CLI_IMAGE:-$(awk '$1 == "FROM" && $3 == "AS" && $4 == "docker-cli" { print $2 }' "$ROOT/tools/lint/images.Dockerfile")}
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
sh "$ROOT/tools/ci/pull.sh" "$KIND_NODE" "$CLI_IMAGE" || {
	echo "FAIL    cannot pull $KIND_NODE or $CLI_IMAGE"
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
# The container this script runs in, if any: its id is in the path of the hostname file Docker mounts into it (this
# holds on the host network too, where the hostname is the host's).
SELF=$(sed -n 's|.*/containers/\([0-9a-f]\{64\}\)/hostname .*|\1|p' /proc/self/mountinfo 2> /dev/null | head -1)
REAPER=$CLUSTER-reaper
# shellcheck disable=SC2329 # run by cleanup, the trap below
nodes() { docker ps -aq --filter "label=io.x-k8s.kind.cluster=$CLUSTER"; }
# shellcheck disable=SC2329 # run by the trap below
cleanup() {
	# kind itself can be gone by now: a cancelled CI job's workspace, where the pinned binary lives, is deleted under
	# the job's containers. The nodes are plain containers, which the Docker CLI removes without it.
	"$KIND" delete cluster --name "$CLUSTER" > "$OUT/delete.log" 2>&1 || {
		nodes | xargs -r docker rm -f -v > /dev/null 2>&1
		echo "FAIL    kind delete cluster $CLUSTER: $(tail -1 "$OUT/delete.log") (its nodes removed with docker rm)"
		FAILED=1
	}
	# The reaper goes only once no node of the cluster is left; otherwise it removes them when this container is gone.
	[ -z "$SELF" ] || [ -n "$(nodes)" ] || docker rm -f -v "$REAPER" > /dev/null 2>&1
	rm -rf "$OUT"
	[ "$FAILED" = 0 ] || exit 1
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Other kind clusters on this Docker host share its inotify instances with this one (see the top of this file).
OTHERS=$(docker ps -a --filter label=io.x-k8s.kind.cluster --format '{{.Names}}' | tr '\n' ' ')
[ -z "$OTHERS" ] || echo "note    other kind nodes on this Docker host: $OTHERS"

echo "cluster $CLUSTER ($KIND_NODE), image $IMAGE"
if [ -n "$SELF" ]; then
	# The Docker host's path of the socket this container was given, for the reaper's own mount.
	SOCK=$(docker inspect --type container -f '{{range .Mounts}}{{if eq .Destination "/var/run/docker.sock"}}{{.Source}}{{end}}{{end}}' "$SELF" 2> /dev/null)
	# The reaper waits while this container runs, then removes whatever nodes of the cluster are left, which is
	# all `kind delete cluster` does on the Docker host. It is removed itself on the way out of this script.
	# shellcheck disable=SC2016 # the reaper's shell expands these
	docker run -d --rm --name "$REAPER" -v "${SOCK:-/var/run/docker.sock}:/var/run/docker.sock" "$CLI_IMAGE" sh -c '
		nodes() { docker ps -aq --filter "label=io.x-k8s.kind.cluster=$1"; }
		while [ "$(docker inspect -f "{{.State.Running}}" "$0" 2> /dev/null)" = true ]; do sleep 5; done
		nodes "$1" | xargs -r docker rm -f -v' "$SELF" "$CLUSTER" > "$OUT/reaper.log" 2>&1 || {
		echo "FAIL    cannot start the reaper $REAPER: $(tail -1 "$OUT/reaper.log")"
		exit 1
	}
else
	echo "note    not in a container: no reaper, a killed run leaves $CLUSTER behind (kind delete cluster --name $CLUSTER)"
fi
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

# A client pod, which no policy selects, reaches the release through its Service's DNS name: Service → endpoints → pod.
# A new cluster's DNS and Service routing (CoreDNS, kube-proxy) can lag the release by a few seconds, so this waits for
# the name to answer before anything is judged through it. If it never does, the cluster is broken, not the chart:
# every check below would fail for that one reason, so the script stops here and shows kube-proxy's last words.
SVC=http://$RELEASE.$NS.svc.cluster.local:8080
kubectl -n "$NS" run client --image "$CURL_IMAGE" --restart Never --command -- sleep 900 > /dev/null
kubectl -n "$NS" wait --for condition=Ready pod/client --timeout 120s > /dev/null || {
	fail "the client pod did not start"
	exit 1
}
START=$(date +%s)
until kubectl -n "$NS" exec client -- curl -fsS --max-time 3 "$SVC/readyz" > "$OUT/readyz" 2>&1; do
	if [ $(($(date +%s) - START)) -ge 90 ]; then
		fail "the cluster's Services do not answer (90 s): $(head -1 "$OUT/readyz")"
		echo "kube-proxy:"
		kubectl -n kube-system get pods -l k8s-app=kube-proxy --no-headers
		kubectl -n kube-system logs -l k8s-app=kube-proxy --tail 3 --previous 2> /dev/null ||
			kubectl -n kube-system logs -l k8s-app=kube-proxy --tail 3
		[ -z "$OTHERS" ] || echo "other kind nodes on this Docker host, which share its inotify instances: $OTHERS"
		exit 1
	fi
	sleep 3
done
ok "GET /readyz through the Service: $(cat "$OUT/readyz") (after $(($(date +%s) - START)) s)"

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

# The client pod converts through the Service.
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
