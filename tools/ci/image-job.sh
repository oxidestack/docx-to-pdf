#!/bin/sh
# The image job (`make image-ci`: build the image for the runner's architecture, then `make image-test`,
# `make image-quickstart` and `make image-installation`, `make image-egress`, `make image-scenarios`,
# `make image-corpus`, `make chart-kind` and the image gates of `make image-lint` against it) on a CI host that runs
# Docker itself: GitHub's runner (.github/workflows/ci.yml), one VM per architecture. Nothing is pushed anywhere: a
# release saves the tested image to a file (IMAGE_SAVE) for another job to push.
#
#   sh tools/ci/image-job.sh host <image>    from the repository root: `make image-ci` in a second container of <image>
#
# The workspace's path is the same for the host and the daemon, and the submodule comes with the checkout. The second
# container runs on the host network with the socket and the workspace mounted at its own path, because the tests
# talk to the containers they start the way a deployment does:
# - image/test.sh and tests/oracle/image-scenarios.sh publish the service on `127.0.0.1::8080` and curl 127.0.0.1.
# - image/test.sh bind-mounts a directory it wrote (the mounted-font test), which the daemon resolves on the host.
# - tools/lint/image.sh runs Trivy, Dockle and syft in containers that bind-mount the workspace at that same path.
# `--init` reaps orphans, as tools/instance-test/run.sh does (tools/ci/test-deps.sh says why that matters). The
# runner's VM goes with a cancelled job, and every container on it.
#
# Environment: IMAGE, the tag to build (default docx-to-pdf-ci:<job>, removed at exit); the workflow's CARGO_*. For a
# release (.github/workflows/release.yml): IMAGE_VERSION and IMAGE_SOURCE, the OCI version and source labels;
# IMAGE_SAVE, a path in the workspace where the image, once every test has passed, is written by `docker save`.
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
	# A release build adds the OCI version and source labels beside the revision (the Makefile's IMAGE_LABELS,
	# extended on make's command line, so every sub-make builds with the same ones).
	if [ -n "${IMAGE_VERSION:-}" ]; then
		# shellcheck disable=SC2016 # $(IMAGE_REVISION) is make's, expanded there
		make image-ci IMAGE="$IMAGE" IMAGE_LABELS='$(if $(IMAGE_REVISION),--label org.opencontainers.image.revision=$(IMAGE_REVISION))'" --label org.opencontainers.image.version=$IMAGE_VERSION${IMAGE_SOURCE:+ --label org.opencontainers.image.source=$IMAGE_SOURCE}"
	else
		make image-ci IMAGE="$IMAGE"
	fi
	# The release workflow publishes the image every test above ran against, not a rebuild: it is saved here, the
	# job having passed, before the trap removes it.
	# Readable by the runner's user, which uploads it (this container runs as root).
	if [ -n "${IMAGE_SAVE:-}" ]; then
		docker save -o "$IMAGE_SAVE" "$IMAGE"
		chmod 644 "$IMAGE_SAVE"
	fi
	exit 0
fi

if [ "${1:-}" = host ]; then
	JOB_IMAGE=${2:-}
	[ -n "$JOB_IMAGE" ] || {
		echo "usage: sh tools/ci/image-job.sh host <image>" >&2
		exit 2
	}
	[ -S "$SOCK" ] || {
		echo "no Docker socket at $SOCK on this host" >&2
		exit 2
	}
	# The Dockerfile copies upstream's fonts: the checkout fetches the submodule (`submodules: true`).
	[ -f upstream/docx-to-pdf/fonts/DroidKufi-Regular.ttf ] || {
		echo "upstream/docx-to-pdf is not checked out: git submodule update --init --depth 1 upstream/docx-to-pdf" >&2
		exit 2
	}
	WS=$PWD
	HOST_SOCK=$SOCK
	JOB=${GITHUB_RUN_ID:-$$}-${GITHUB_RUN_ATTEMPT:-1}-$(uname -m)
else
	echo "usage: sh tools/ci/image-job.sh host <image>" >&2
	exit 2
fi
IMAGE=${IMAGE:-docx-to-pdf-ci:$JOB}
NAME=docx-to-pdf-ci-image-$JOB

# The container this starts outlives this script: on the way out, by an exit or a signal, remove it and the image.
cleanup() {
	docker rm -f -v "$NAME" > /dev/null 2>&1 || true
	docker image rm -f "$IMAGE" > /dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

echo "image job: $IMAGE in $NAME ($JOB_IMAGE, workspace $WS)"
docker run --rm --init --name "$NAME" --network host \
	-v "$HOST_SOCK:$SOCK" -v "$WS:$WS" -w "$WS" \
	-e CARGO_HOME="$WS/.cargo" -e CARGO_TERM_COLOR -e CARGO_INCREMENTAL -e CARGO_NET_RETRY -e IMAGE="$IMAGE" \
	-e IMAGE_VERSION -e IMAGE_SOURCE -e IMAGE_SAVE \
	"$JOB_IMAGE" sh tools/ci/image-job.sh inner
