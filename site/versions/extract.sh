#!/bin/sh
# The first half of `make site-versions` (site/README.md, "Versions"), on the host since it needs git: the releases
# whose documentation the site publishes, each tag's tree unpacked under target/site/versions/<tag>/, and their list in
# target/site/versions/tags, oldest first. build.sh, in the node image, builds them.
#
# A release is a tag vX.Y.Z: a pre-release (vX.Y.Z-rc.N) has no documentation version, its notes are its GitHub
# release's. One version per minor, built from that minor's latest patch: 1.1 is v1.1.3 once v1.1.3 is out.
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
WORK=$ROOT/target/site/versions

tags=$(git -C "$ROOT" tag --list 'v*' | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | sort -V |
	awk -F. '{ minor = $1 "." $2; if (!(minor in last)) order[++n] = minor; last[minor] = $0 }
		END { for (i = 1; i <= n; i++) print last[order[i]] }') || true
[ -n "$tags" ] || {
	echo "site-versions: no release tag vX.Y.Z, so no documentation to publish" >&2
	exit 1
}

rm -rf "$WORK"
mkdir -p "$WORK"
for tag in $tags; do
	mkdir "$WORK/$tag"
	git -C "$ROOT" archive "$tag" | tar -x -C "$WORK/$tag"
	echo "$tag" >> "$WORK/tags"
done
echo "site-versions: $(echo $tags | tr ' ' ',')" >&2
