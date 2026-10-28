#!/bin/sh
# The image gates: the Dockerfiles through BuildKit's own checks, then the built image
# through Trivy, Dockle, a size budget and syft. They need a Docker daemon and, but for `build-check`, the built
# image, so they are not tools/lint/lint.sh's gates: `make image-ci` runs them after `make image`, which is
# the private CI's `image` job, on its runner with a Docker socket. Each tool is a stage of
# tools/lint/images.Dockerfile, pinned by the digest of its multi-arch index, so the arm64 run here and an amd64 run
# in GitHub Actions (0110) use the same tool.
#
#   sh tools/lint/image.sh [IMAGE] [<gate>[,<gate>]...]    (`make image-lint [IMAGE=] [ONLY=]`; IMAGE docx-to-pdf:dev)
#   sh tools/lint/image.sh --record-size [IMAGE]           write IMAGE's size as its architecture's baseline
#
# Gates (in this order):
#   build-check  `docker buildx build --check` on every Dockerfile hadolint lints: BuildKit's own rules, through the
#                pinned `docker-cli` stage's buildx (needs the daemon's /var/run/docker.sock).
#   trivy        fixable HIGH/CRITICAL vulnerabilities in the Debian packages and the Rust crates (cargo-auditable
#                puts them in the binary), secrets in the layers, misconfigurations of the image config.
#                Ignored findings are .trivyignore.yaml's, each with a `statement:` and an `expired_at:`.
#   dockle       the CIS Docker benchmark and Dockle's own rules, failing at INFO; .dockleignore names an accepted
#                rule with its reason.
#   size         the image (its layers' uncompressed bytes, read from `docker save`: the classic store saves them
#                plain and the containerd store gzipped, so the archive's own size depends on the daemon) against the baseline of
#                its architecture in tools/lint/image-size.txt: more than IMAGE_SIZE_GROWTH percent (10) over it
#                fails. The baseline is the last release's; `--record-size` moves it, deliberately.
#   sbom         syft's SBOM of the image, SPDX and CycloneDX JSON, into target/sbom/ (the release ships them).
#
# Every gate runs even after one fails. Exit 0: all passed; 1: one or more failed; 2: a usage error or no image.
# Containers are `docker run --rm --name docx-to-pdf-image-lint-<gate>-<pid>` with the repository mounted at its own
# path (so the same command works in the private CI image job's container, whose workspace is at its path on the Docker host),
# and the saved image goes to target/lint/image-<pid>/, removed at exit.
set -eu

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
IMAGES="$ROOT/tools/lint/images.Dockerfile"
SIZES="$ROOT/tools/lint/image-size.txt"
GATES="build-check trivy dockle size sbom"
GROWTH=${IMAGE_SIZE_GROWTH:-10}
WORK="$ROOT/target/lint/image-$$"
TAR="$WORK/image.tar"

usage() {
	echo "usage: sh tools/lint/image.sh [IMAGE] [<gate>[,<gate>]...] | --record-size [IMAGE]   (gates: $GATES)" >&2
	exit 2
}

image_ref() {
	ref=$(awk -v s="$1" 'toupper($1) == "FROM" && toupper($3) == "AS" && $4 == s { print $2 }' "$IMAGES")
	case "$ref" in
	*@sha256:*) echo "$ref" ;;
	*)
		echo "image-lint: stage '$1' in $IMAGES is missing or not pinned by digest" >&2
		return 2
		;;
	esac
}

# Each gate runs in a subshell; a container's name carries this script's pid, so the exit trap finds it.
cleanup() {
	for s in buildx trivy dockle syft; do docker rm -f -v "docx-to-pdf-image-lint-$s-$$" > /dev/null 2>&1 || true; done
	rm -rf "$WORK"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# The CI runner's network drops a large download now and then (Trivy's DB from mirror.gcr.io: "unexpected EOF",
# pipeline of 3dec3a2). A download, and only a download, is tried up to four times, 2, 4 and 8 s apart, as
# tools/lint/fetch-tool.sh does; a gate's verdict is never retried.
#   retry <what> <command>...
# A subshell, so its counters stay its own whatever the command sets.
retry() (
	what=$1
	shift
	attempt=1 delay=2
	while :; do
		"$@" && exit 0
		[ "$attempt" -lt 4 ] || {
			echo "image-lint: $what failed $attempt times" >&2
			exit 1
		}
		echo "image-lint: attempt $attempt to $what failed; retrying in ${delay}s" >&2
		sleep "$delay"
		attempt=$((attempt + 1)) delay=$((delay * 2))
	done
)

# A pinned tool image, pulled with retries (tools/ci/pull.sh) before its `docker run`, which would pull it once, unretried.
pulled() { sh "$ROOT/tools/ci/pull.sh" "$1"; }

# A tool's container: the repository at its own path, the working directory its root.
tool() {
	stage=$1
	shift
	ref=$(image_ref "$stage") && pulled "$ref" || return 2
	docker run --rm --name "docx-to-pdf-image-lint-$stage-$$" -v "$ROOT:$ROOT" -w "$ROOT" "$ref" "$@"
}

# `docker buildx build` in the pinned CLI's container, given the daemon's socket.
buildx() {
	ref=$(image_ref docker-cli) && pulled "$ref" || return 2
	docker run --rm --name "docx-to-pdf-image-lint-buildx-$$" -v /var/run/docker.sock:/var/run/docker.sock \
		-v "$ROOT:$ROOT" -w "$ROOT" "$ref" docker buildx build "$@"
}

# `docker save` once; trivy, dockle, size and syft read the archive, so none of them needs the Docker socket.
saved() {
	[ -s "$TAR" ] && return 0
	mkdir -p "$WORK"
	docker save -o "$TAR" "$IMAGE"
}

arch() { docker image inspect -f '{{.Architecture}}' "$IMAGE"; }
# The layers' uncompressed bytes: manifest.json names them; each is a tar, plain or gzipped.
tar_bytes() {
	layers=$(tar -xOf "$TAR" manifest.json | sed -n 's/.*"Layers":\[\([^]]*\)\].*/\1/p' | tr ',' ' ' | tr -d '"')
	[ -n "$layers" ] || {
		echo "image-lint: no layers in the saved $IMAGE" >&2
		return 1
	}
	rm -rf "$WORK/layers" && mkdir -p "$WORK/layers"
	# shellcheck disable=SC2086 # a list of paths in the archive
	tar -xf "$TAR" -C "$WORK/layers" $layers
	total=0
	for l in $layers; do
		f="$WORK/layers/$l"
		if [ "$(od -An -tx1 -N2 "$f" | tr -d ' \n')" = 1f8b ]; then
			n=$(gzip -dc "$f" | wc -c)
		else
			n=$(wc -c < "$f")
		fi
		total=$((total + n))
	done
	rm -rf "$WORK/layers"
	echo "$total"
}

gate_build_check() {
	# Each Dockerfile with the context and the named build contexts its build uses (Makefile, tools/*/run.sh), so
	# a `COPY --from=<context>` resolves. --check evaluates the rules and builds nothing.
	# The pinned CLI of images.Dockerfile's `docker-cli` stage, not the caller's: `--check` needs buildx 0.15, and
	# the private CI image job's container has Debian trixie's older one ("unknown flag: --check", 0132b).
	# Its socket is the daemon's own /var/run/docker.sock (a Docker VM's, the CI runner's host).
	rc=0
	buildx --check -f "$ROOT/Dockerfile" "$ROOT" || rc=1
	buildx --check -f "$ROOT/tools/bench/Dockerfile" "$ROOT/tools/bench" || rc=1
	buildx --check -f "$ROOT/tools/compare/Dockerfile" "$ROOT/tools/compare" || rc=1
	buildx --check --build-context ci="$ROOT/tools/ci" \
		--build-context fonts="$ROOT/upstream/docx-to-pdf/fonts" \
		--build-context image-fontconfig="$ROOT/image/fontconfig" \
		-f "$ROOT/tools/instance-test/Dockerfile" "$ROOT/tools/instance-test" || rc=1
	# One stage per tool and no default target: --check reads every stage anyway.
	buildx --check -f "$ROOT/tools/lint/images.Dockerfile" "$ROOT/tools/lint" || rc=1
	return $rc
}

gate_trivy() {
	# An ignore entry without its reason or its expiry is a finding of its own.
	if ! awk '
		function close_entry() { if (id != "" && (!st || !ex)) { print "image-lint: .trivyignore.yaml: " id " needs a statement: and an expired_at:"; bad = 1 } }
		/^[[:space:]]*- id:/ { close_entry(); id = $3; st = 0; ex = 0; next }
		/^[[:space:]]*statement:/ { st = 1 }
		/^[[:space:]]*expired_at:/ { ex = 1 }
		END { close_entry(); exit bad }' "$ROOT/.trivyignore.yaml"; then
		return 1
	fi
	saved
	# The vulnerability DB first, on its own and retried (a no-op while the cached one is fresh); then the scan, once,
	# offline for the DB, so a finding fails at once and is never retried into a pass. The scan still fetches the
	# misconfiguration checks bundle, unretried: Trivy falls back to its embedded checks when that fails.
	cache="$ROOT/target/lint/trivy"
	retry "download Trivy's vulnerability DB" \
		tool trivy image --download-db-only --cache-dir "$cache" --quiet --skip-version-check
	tool trivy image --input "$TAR" --cache-dir "$cache" --quiet --skip-version-check --skip-db-update \
		--scanners vuln,secret --image-config-scanners misconfig,secret \
		--severity HIGH,CRITICAL --ignore-unfixed --ignorefile .trivyignore.yaml --exit-code 1
}

gate_dockle() {
	saved
	# .dockleignore is read from the working directory, the repository root.
	tool dockle --input "$TAR" --exit-code 1 --exit-level info
}

gate_size() {
	saved
	a=$(arch)
	bytes=$(tar_bytes)
	base=$(awk -v a="$a" '$1 == a { print $2 }' "$SIZES")
	if [ -z "$base" ]; then
		echo "image-lint: no $a baseline in tools/lint/image-size.txt ($bytes bytes now): record it with" \
			"\`make image-size-record\` on a release build" >&2
		return 1
	fi
	limit=$((base + base * GROWTH / 100))
	echo "image-lint: size $a $bytes bytes, baseline $base, limit $limit (+$GROWTH%)"
	[ "$bytes" -le "$limit" ] || {
		echo "image-lint: $a image grew $(((bytes - base) * 100 / base))% over the baseline" >&2
		return 1
	}
}

gate_sbom() {
	saved
	a=$(arch)
	out="$ROOT/target/sbom"
	mkdir -p "$out"
	rm -f "$out/docx-to-pdf-$a.spdx.json" "$out/docx-to-pdf-$a.cdx.json"
	tool syft scan "docker-archive:$TAR" --source-name docx-to-pdf -q \
		-o "spdx-json=$out/docx-to-pdf-$a.spdx.json" -o "cyclonedx-json=$out/docx-to-pdf-$a.cdx.json" || return 1
	# The SBOM is only worth shipping if it sees both halves of the image: the Debian packages and the crates.
	rc=0
	for f in "$out/docx-to-pdf-$a.spdx.json" "$out/docx-to-pdf-$a.cdx.json"; do
		for p in libreoffice-core-nogui tokio; do
			grep -q "\"$p\"" "$f" || {
				echo "image-lint: $f lists no $p" >&2
				rc=1
			}
		done
	done
	[ $rc -ne 0 ] || echo "image-lint: SBOM in $out/docx-to-pdf-$a.{spdx,cdx}.json"
	return $rc
}

command -v docker > /dev/null || {
	echo "image-lint: needs docker" >&2
	exit 2
}

if [ "${1:-}" = --record-size ]; then
	IMAGE=${2:-docx-to-pdf:dev}
	docker image inspect "$IMAGE" > /dev/null || exit 2
	saved
	a=$(arch)
	bytes=$(tar_bytes)
	{
		grep -v "^$a " "$SIZES" 2> /dev/null || true
		echo "$a $bytes"
	} > "$SIZES.new"
	mv "$SIZES.new" "$SIZES"
	echo "image-lint: $a baseline $bytes bytes in tools/lint/image-size.txt"
	exit 0
fi
case "${1:-}" in -*) usage ;; esac

IMAGE=${1:-docx-to-pdf:dev}
selected=$(echo "${2:-$GATES}" | tr ',' ' ')
for g in $selected; do case " $GATES " in *" $g "*) ;; *)
	echo "image-lint: no gate '$g'" >&2
	usage
	;;
esac done
case " $selected " in
" build-check ") ;;
*) docker image inspect "$IMAGE" > /dev/null || {
	echo "image-lint: no image $IMAGE (make image)" >&2
	exit 2
} ;;
esac

failed=""
for g in $selected; do
	echo "--- image-lint: $g ($IMAGE)"
	set +e
	(
		set -e
		"gate_$(echo "$g" | tr - _)"
	)
	code=$?
	set -e
	if [ "$code" -eq 0 ]; then
		echo "--- image-lint: $g passed"
	else
		echo "--- image-lint: $g FAILED (exit $code)"
		failed="$failed $g"
	fi
done

if [ -n "$failed" ]; then
	echo "image-lint: failed:$failed"
	exit 1
fi
echo "image-lint: passed: $selected"
