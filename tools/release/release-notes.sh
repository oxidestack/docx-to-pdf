#!/bin/sh
# Drafts the next release's section of CHANGELOG.md from the commits since the last tag (`make release-notes`): the
# pinned git-cliff of tools/lint/images.Dockerfile's `git-cliff` stage, in its container, with cliff.toml. The version
# is the one the commits call for (fix a patch, feat a minor, breaking a major; 1.0.0 before the first tag), and the
# section goes under CHANGELOG.md's head. The draft is edited by hand before the tag (tools/release/README.md).
#
#   sh tools/release/release-notes.sh [<repository>]     default: this one
#
# The repository is mounted into the container, so on macOS it lies where the Docker VM sees it (the home directory,
# by default); from inside a container, see RELEASE_NOTES_VOLUMES_FROM below. Exit 0: drafted, the version printed; 1: git-cliff failed; 2: no Docker, no such repository, or no
# CHANGELOG.md in it.
set -eu

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
REPO=${1:-$ROOT}
[ -d "$REPO" ] || {
	echo "release-notes: no directory $REPO" >&2
	exit 2
}
REPO=$(cd "$REPO" && pwd)
[ -f "$REPO/CHANGELOG.md" ] || {
	echo "release-notes: no CHANGELOG.md in $REPO" >&2
	exit 2
}
command -v docker > /dev/null || {
	echo "release-notes: needs Docker" >&2
	exit 2
}
IMAGE=$(awk '$1 == "FROM" && $3 == "AS" && $4 == "git-cliff" { print $2 }' "$ROOT/tools/lint/images.Dockerfile")
[ -n "$IMAGE" ] || {
	echo "release-notes: no git-cliff stage in tools/lint/images.Dockerfile" >&2
	exit 2
}

# The image's entrypoint runs git-cliff as the owner of the working directory, so the files it writes are the caller's.
#
# RELEASE_NOTES_VOLUMES_FROM=<container>: the caller is itself a container talking to the host's Docker socket (a CI
# job). A bind mount's path is resolved on the Docker host, where the job's paths do not exist, so git-cliff gets that
# container's volumes instead, at the same paths, and reads the repository and cliff.toml where the caller sees them.
run() {
	if [ -n "${RELEASE_NOTES_VOLUMES_FROM:-}" ]; then
		docker run --rm --name "docx-to-pdf-release-notes-$$" \
			--volumes-from "$RELEASE_NOTES_VOLUMES_FROM" -w "$REPO" \
			"$IMAGE" --config "$ROOT/cliff.toml" "$@"
	else
		docker run --rm --name "docx-to-pdf-release-notes-$$" \
			-v "$ROOT/cliff.toml:/cliff.toml:ro" -v "$REPO:/repo" -w /repo \
			"$IMAGE" --config /cliff.toml "$@"
	fi
}

version=$(run --bumped-version) || exit 1
run --unreleased --bump --prepend CHANGELOG.md || exit 1
echo "release-notes: drafted $version at the top of $REPO/CHANGELOG.md"
