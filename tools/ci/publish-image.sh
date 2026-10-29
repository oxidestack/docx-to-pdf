#!/bin/sh
# Publishes the release image: the per-architecture images the image job tested, saved by `docker save`, pushed by
# digest into one multi-arch manifest list, tagged. tools/ci/release.sh signs it and copies it, with its signatures and
# attestations, to the mirror. .github/workflows/release.yml runs both; crane does the registry work here, from its
# image pinned below, so nothing is installed on the host. Docker is the only requirement.
#
#   sh tools/ci/publish-image.sh push   <repository> <version> <image.tar>...
#       Pushes each tarball by its digest (no per-architecture tag is left behind), appends them to one manifest list
#       and tags it <version>, and `latest` too unless <version> is a pre-release (it has a `-`). Prints the list's
#       digest on the last line of stdout.
#   sh tools/ci/publish-image.sh version <tag>
#       Prints the version a release tag names (v1.2.3 → 1.2.3, v1.3.0-rc.1 → 1.3.0-rc.1); exit 2 for any other tag.
#   sh tools/ci/publish-image.sh tags <version>
#       The tags a version publishes: <version>, and `latest` unless it is a pre-release.
#
# Every registry call is tried three times, 10 s then 30 s apart: a dropped connection is not a failed release. What
# fails all three exits 1, which fails the release.
#
# Environment: CRANE_CONFIG, the directory of the registry credentials (a Docker config.json, which
# `tools/ci/release.sh login` writes; default $DOCKER_CONFIG or ~/.docker), mounted into crane's container read-only;
# CRANE_FLAGS, extra flags for every crane call (`--insecure` for a plain-HTTP test registry).
# Exit 0: done; 1: a registry call failed; 2: usage, or a tarball is missing.
set -eu

# crane v0.22.1, the manifest list's digest (linux/amd64 and linux/arm64).
CRANE=gcr.io/go-containerregistry/crane:v0.22.1@sha256:1f968817b95790bed063f71175aa6b8ff879fa17064020415f3e18bb6e6a36e1

CRANE_CONFIG=${CRANE_CONFIG:-${DOCKER_CONFIG:-$HOME/.docker}}

usage() {
	sed -n '7,14p' "$0" >&2
	exit 2
}

# crane <args>: one crane call in its container on the host network (a test registry on localhost is reachable),
# with the credentials, and the tarballs' directory when $TARS is set, mounted read-only.
crane() {
	mkdir -p "$CRANE_CONFIG"
	[ -f "$CRANE_CONFIG/config.json" ] || printf '{}\n' > "$CRANE_CONFIG/config.json"
	# shellcheck disable=SC2086 # CRANE_FLAGS is a list of flags
	docker run --rm -i --network host -e DOCKER_CONFIG=/crane-config -v "$CRANE_CONFIG:/crane-config:ro" \
		${TARS:+-v "$TARS:/tars:ro"} "$CRANE" "$@" ${CRANE_FLAGS:-}
}

# retry <what> <command>...: the command up to three times, 10 s then 30 s apart.
retry() {
	what=$1
	shift
	for delay in 10 30 0; do
		"$@" && return 0
		[ "$delay" = 0 ] && break
		echo "publish-image: $what failed; again in ${delay}s" >&2
		sleep "$delay"
	done
	echo "publish-image: $what failed three times" >&2
	return 1
}

# `latest` moves only with a release: a pre-release (1.1.0-rc.1) leaves it where it was.
tags_for() {
	case $1 in
	*-*) echo "$1" ;;
	*) echo "$1 latest" ;;
	esac
}

[ $# -ge 1 ] || usage
cmd=$1
shift
case $cmd in
version)
	[ $# -eq 1 ] || usage
	# SemVer's core and pre-release; no build metadata, which a registry tag cannot carry (`+`).
	v=${1#v}
	if [ "v$v" = "$1" ] && printf '%s\n' "$v" | grep -Eqx '(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?'; then
		echo "$v"
	else
		echo "publish-image: $1 is not a release tag (v<major>.<minor>.<patch>[-<pre-release>])" >&2
		exit 2
	fi
	;;
tags)
	[ $# -eq 1 ] || usage
	tags_for "$1"
	;;
push)
	[ $# -ge 3 ] || usage
	repo=$1 version=$2
	shift 2
	TARS=""
	manifests=""
	for tar in "$@"; do
		[ -f "$tar" ] || {
			echo "publish-image: no tarball $tar" >&2
			exit 2
		}
		dir=$(cd "$(dirname "$tar")" && pwd)
		[ -z "$TARS" ] || [ "$TARS" = "$dir" ] || {
			echo "publish-image: the tarballs must share one directory" >&2
			exit 2
		}
		TARS=$dir
		name=/tars/$(basename "$tar")
		# The digest crane will push the image under, computed from the tarball the same way the push builds it, so
		# the push names the digest and leaves no tag of its own.
		digest=$(crane digest --tarball "$name")
		retry "push of $tar" crane push "$name" "$repo@$digest" > /dev/null
		echo "publish-image: $tar → $repo@$digest" >&2
		manifests="$manifests -m $repo@$digest"
	done
	# shellcheck disable=SC2046 # the tags are words
	set -- $(tags_for "$version")
	first=$1
	shift
	# A Docker manifest list, as the images are Docker manifests (`docker save`'s); each entry's platform is read
	# from its image's config.
	# shellcheck disable=SC2086 # $manifests is a list of flags
	retry "manifest list $repo:$first" crane index append --docker-empty-base $manifests -t "$repo:$first" > /dev/null
	for tag in "$@"; do
		retry "tag $repo:$tag" crane tag "$repo:$first" "$tag"
	done
	retry "digest of $repo:$first" crane digest "$repo:$first"
	;;
*) usage ;;
esac
