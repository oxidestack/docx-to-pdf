#!/bin/sh
# The image job (`make image-ci`: build the image for the runner's architecture, then `make image-test`,
# `make image-quickstart` and `make image-installation`, `make image-egress`, `make image-scenarios`,
# `make image-corpus`, `make chart-kind` and the image gates of `make image-lint` against it) in a
# GitRiver job container that its runner has given the host's Docker socket (the `image` job of
# .gitriver/workflows/ci.yml names that runner). No other runner gives one, and nothing is pushed
# anywhere.
#
#   sh tools/ci/image-job.sh          from the repository root, in the job's container (`rust:1.98.0-trixie`)
#
# The socket is root on the Docker host (a VM other projects share): whatever this script, `make image`
# or the tests start runs beside every other container there. Everything it starts is named after the job and
# removed at exit, and the image it builds is removed too; the daemon's build cache is left alone, because pruning
# it would prune every project's.
#
# Why it starts a second container rather than running `make image-ci` where it is. A job container is a sibling
# of the containers it starts, on Docker's default bridge, and the tests talk to them the way a deployment does:
# - image/test.sh and tests/oracle/image-scenarios.sh publish the service on `127.0.0.1::8080` and curl
#   127.0.0.1. That is the VM's loopback, which a bridge container cannot reach; on the host network it can.
# - image/test.sh bind-mounts a directory it wrote (the mounted-font test). The daemon resolves that path on the
#   VM, where the job's `/workspace` is `<the runner's workdir>/<job>`, so the path must be the same on both sides.
# - tools/lint/image.sh runs Trivy, Dockle and syft in containers that bind-mount the workspace at that same path.
# So this runs `make image-ci` in a container of the job's own image, with the job's own memory limit and cgroup
# parent (the CI slice), on the host network, with the job's workspace mounted at its path on the VM and the
# socket. `--init` reaps orphans, as tools/instance-test/run.sh does (tools/ci/test-deps.sh says why that matters).
#
# Environment: IMAGE, the tag to build (default docx-to-pdf-ci:<job>, removed at exit); the workflow's CARGO_*.
set -eu

SOCK=/var/run/docker.sock

# apt from the snapshot tools/ci/test-deps.sh pins, read from there so the date lives in one place. That script's
# `base` mode (the submodule) does not set apt up, so this does.
apt_install() {
	# shellcheck disable=SC2016 # the pattern matches a literal ${SNAPSHOT:-…}
	snapshot=$(sed -n 's/^SNAPSHOT=${SNAPSHOT:-\([0-9TZ]*\)}$/\1/p' tools/ci/test-deps.sh)
	[ -n "$snapshot" ] || {
		echo "no SNAPSHOT= line in tools/ci/test-deps.sh" >&2
		exit 2
	}
	rm -f /etc/apt/sources.list.d/* /etc/apt/sources.list
	printf 'Types: deb\nURIs: http://snapshot.debian.org/archive/debian/%s/\nSuites: trixie trixie-updates\nComponents: main\nSigned-By: /usr/share/keyrings/debian-archive-keyring.gpg\n' "$snapshot" \
		> /etc/apt/sources.list.d/snapshot.sources
	apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Retries=3 update -qq
	apt-get install -y -qq --no-install-recommends -o Acquire::Retries=3 "$@" > /dev/null
}

if [ "${1:-}" = inner ]; then
	# In the second container: the Docker CLI and buildx (`make image` is `docker buildx build`), poppler-utils
	# (image/test.sh reads the fonts a PDF embeds with pdffonts) and the Compose plugin (`make image-installation`
	# needs 2.24 or later; the snapshot's docker-compose is 2.26.1, a CLI plugin). curl is in the job's image; helm is
	# fetched pinned (tools/lint/tools.txt). The submodule is already in the workspace.
	apt_install docker-cli docker-buildx docker-compose poppler-utils
	docker version --format 'docker client {{.Client.Version}}, daemon {{.Server.Version}} ({{.Server.Arch}})'
	trap 'docker image rm -f "$IMAGE" > /dev/null 2>&1 || true' EXIT INT TERM
	# `make image` labels the image with the checkout's commit (the Makefile's IMAGE_REVISION). This is a fresh
	# container, so the first one's safe.directory (tools/ci/test-deps.sh) is not here: without it git refuses a
	# workspace owned by another uid, and the image would be built with no revision label at all.
	git config --global --add safe.directory '*'
	make image-ci IMAGE="$IMAGE"
	exit 0
fi

[ -S "$SOCK" ] || {
	echo "no Docker socket at $SOCK: this job runs only on the runner that gives its jobs the socket" >&2
	exit 2
}

# The submodule (the Dockerfile copies upstream's fonts), into the workspace both containers share; then the CLI
# only, to read this container's own settings and start the second one.
sh tools/ci/test-deps.sh base
apt_install docker-cli

# A container's hostname is its short id unless the runner sets one; it does not (2026-10-21, `docker inspect`).
SELF=$(hostname)
mount_of() {
	docker inspect --type container -f "{{range .Mounts}}{{if eq .Destination \"$1\"}}{{.Source}}{{end}}{{end}}" "$SELF"
}
WS=$(mount_of /workspace)
HOST_SOCK=$(mount_of "$SOCK")
[ -n "$WS" ] && [ -n "$HOST_SOCK" ] || {
	echo "cannot find this job's /workspace and socket mounts on the Docker host (container $SELF)" >&2
	exit 2
}
JOB_IMAGE=$(docker inspect --type container -f '{{.Config.Image}}' "$SELF")
MEMORY=$(docker inspect --type container -f '{{.HostConfig.Memory}}' "$SELF")
CGROUP=$(docker inspect --type container -f '{{.HostConfig.CgroupParent}}' "$SELF")
JOB=${CI_JOB_ID:-$SELF}
IMAGE=${IMAGE:-docx-to-pdf-ci:$JOB}
NAME=docx-to-pdf-ci-image-$JOB

limits=""
[ "$MEMORY" = 0 ] || limits="--memory $MEMORY --memory-swap $MEMORY"
[ -z "$CGROUP" ] || limits="$limits --cgroup-parent $CGROUP"

# A cancelled job stops this script, not the container it started: remove it, and the image, on the way out.
cleanup() {
	docker rm -f -v "$NAME" > /dev/null 2>&1 || true
	docker image rm -f "$IMAGE" > /dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

echo "image job: $IMAGE in $NAME ($JOB_IMAGE, workspace $WS, limits:${limits:- none})"
# shellcheck disable=SC2086 # $limits is a list of options
docker run --rm --init --name "$NAME" --network host $limits \
	-v "$HOST_SOCK:$SOCK" -v "$WS:$WS" -w "$WS" \
	-e CARGO_HOME="$WS/.cargo" -e CARGO_TERM_COLOR -e CARGO_INCREMENTAL -e CARGO_NET_RETRY -e IMAGE="$IMAGE" \
	"$JOB_IMAGE" sh tools/ci/image-job.sh inner
