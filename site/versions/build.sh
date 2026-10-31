#!/bin/sh
# The second half of `make site-versions` (site/README.md, "Versions"), in the pinned node image: each release that
# extract.sh unpacked is built by its own site/build.sh, with its own lockfile and pages, under
# /docx-to-pdf/<x.y>/, and the latest once more at the root, /docx-to-pdf/. Then inject.mjs gives every page the
# version picker and, on an older release, the banner that points at the latest. The site lands in target/site/pages/.
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
WORK=$ROOT/target/site/versions
OUT=$ROOT/target/site/pages
SITE_ROOT=/docx-to-pdf

# build <tree> <tag> <base> <out>: the release's site built at <base> into <out>.
build() {
	tree=$1 tag=$2 base=$3 out=$4
	if grep -q SITE_BASE "$tree/site/astro.config.mjs"; then
		:
	else
		# A release before the base was a setting (1.0, 1.1) writes it in three places (the config, the staging and the
		# Benchmarks page's links); the build sets it there.
		sed -i "s#^  base: '[^']*',#  base: '$base',#" "$tree/site/astro.config.mjs"
		sed -i "s#^const BASE = '[^']*'#const BASE = '$base'#" "$tree/site/scripts/stage.mjs"
		sed -i "s#](/docx-to-pdf[^/]*\(/[0-9.]*\)\{0,1\}/benchmarks/#]($base/benchmarks/#g" "$tree/site/scripts/evidence.mjs"
		# Their README showed the mark from a file outside the repository that is no longer public; the header
		# shows the mark anyway.
		sed -i '/^!\[[^]]* logo\](https:/d' "$tree/README.md"
	fi
	SITE_BASE=$base SITE_REF=$tag sh "$tree/site/build.sh"
	rm -rf "$out"
	mkdir -p "$(dirname "$out")"
	cp -R "$tree/site/dist" "$out"
}

rm -rf "$OUT"
mkdir -p "$OUT"
latest=$(tail -n 1 "$WORK/tags")
versions=
while read -r tag; do
	minor=${tag#v}
	minor=${minor%.*}
	versions="$minor $versions"
	build "$WORK/$tag" "$tag" "$SITE_ROOT/$minor" "$OUT/$minor"
	if [ "$tag" = "$latest" ]; then
		root=$WORK/root
		build "$WORK/$tag" "$tag" "$SITE_ROOT" "$root"
		cp -R "$root"/. "$OUT"/
		rm -rf "$root"
	fi
done < "$WORK/tags"

# shellcheck disable=SC2086 # the versions are words, newest first
node "$ROOT/site/versions/inject.mjs" "$OUT" "$SITE_ROOT" "https://github.com/oxidestack/docx-to-pdf" $versions
