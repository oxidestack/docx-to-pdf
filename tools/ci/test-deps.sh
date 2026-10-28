#!/bin/sh
# Turns a `rust:1.98.0-trixie` CI job container into the environment tools/instance-test/Dockerfile
# builds, so the `test` job runs every test the way `tools/instance-test/run.sh` does: the LibreOfficeKit tests
# required (DOCX_TO_PDF_REQUIRE_LOK=1, set by the job), moalhaddar/docx-to-pdf scenarios converting for real, PDF
# equivalence through poppler-utils. A private CI job cannot build or run a container (no Docker socket), so
# the image's steps run here, in the job's own container, as root.
#
#   sh tools/ci/test-deps.sh [base|full]     from the repository root, as root, in a Debian trixie container
#
# `base` (the image job): the submodule. `full` (the default, the `test` job): base, then
# LibreOffice, the fonts, fontconfig and poppler-utils.
#
# No init: a job container's PID 1 does not reap, so an orphan of a killed instance group stays a zombie in the
# group; the instance and pool tests count a zombie as gone (src/instance/driver.rs `group_alive`).
#
# This is the one list, so the CI job and the local instance-test image cannot drift apart.
# tools/instance-test/Dockerfile runs this script too (`full`), from a copy of the paths it reads assembled outside any
# git checkout; there the submodule step is skipped and the fonts are taken as given. A package, font or fontconfig
# file changes here only.
#
# The runner clones depth 1 and never fetches submodules, and the tests read upstream/docx-to-pdf (its examples
# and fonts), so in a git checkout the submodule is fetched first at the commit the superproject pins (1.9 MB from
# GitHub).
set -eu

MODE=${1:-full}
SNAPSHOT=${SNAPSHOT:-20261026T202550Z}
case "$MODE" in base | full) ;; *)
	echo "usage: sh tools/ci/test-deps.sh [base|full]" >&2
	exit 2
	;;
esac

# The checkout may belong to another uid than the job's root (a local run of a CI job copies the host's files); git refuses
# such a tree without this. The job container is thrown away afterwards.
if [ -e .git ]; then
	git config --global --add safe.directory '*'
	git submodule update --init --depth 1 upstream/docx-to-pdf
	echo "upstream/docx-to-pdf at $(git -C upstream/docx-to-pdf rev-parse HEAD)"
else
	echo "not a git checkout: upstream/docx-to-pdf as given (tools/instance-test/Dockerfile)"
fi
[ "$MODE" = full ] || exit 0

rm -f /etc/apt/sources.list.d/* /etc/apt/sources.list
printf 'Types: deb\nURIs: http://snapshot.debian.org/archive/debian/%s/\nSuites: trixie trixie-updates trixie-backports\nComponents: main\nSigned-By: /usr/share/keyrings/debian-archive-keyring.gpg\n' "$SNAPSHOT" \
	> /etc/apt/sources.list.d/snapshot.sources
apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Retries=3 update -qq
# libreoffice-uiconfig-writer, which writer-nogui does not pull in; without
# modules/swriter/ui/annotation.ui a LOK load of any document with a comment fails ("empty reference").
apt-get install -y -qq --no-install-recommends -o Acquire::Retries=3 -t trixie-backports \
	libreoffice-writer-nogui libreoffice-core-nogui libreoffice-math-nogui libreoffice-uiconfig-writer \
	fonts-opensymbol > /dev/null
apt-get install -y -qq --no-install-recommends -o Acquire::Retries=3 \
	fontconfig fonts-crosextra-carlito fonts-crosextra-caladea fonts-dejavu fonts-liberation \
	fonts-liberation2 fonts-liberation-sans-narrow fonts-linuxlibertine fonts-noto-core fonts-noto-extra \
	fonts-noto-mono fonts-noto-ui-core fonts-sil-gentium fonts-sil-gentium-basic poppler-utils > /dev/null
# LibreOffice's shared extension folder, outside every profile. Without it, two first
# starts as root race to create it and the loser exits 1 (NameClashException); with it, the creation is skipped.
mkdir -p /var/spool/libreoffice/uno_packages/cache/uno_packages

mkdir -p /usr/share/fonts/custom
cp upstream/docx-to-pdf/fonts/DroidKufi-Regular.ttf upstream/docx-to-pdf/fonts/DroidKufi-Bold.ttf \
	upstream/docx-to-pdf/fonts/Cantarell-ExtraBold.otf /usr/share/fonts/custom/
cp tools/instance-test/fontconfig/09-docx-to-pdf-drop-lo-generic.conf /etc/fonts/conf.d/
cp tools/instance-test/fontconfig/local.conf /etc/fonts/local.conf
cp image/fontconfig/52-docx-to-pdf-system-ui.conf /etc/fonts/conf.d/
# An image layer keeps directory mtimes to whole seconds, fontconfig's cache to the nanosecond; round
# each font directory's mtime down to its second before the cache is written, or in the instance-test image every
# system cache is stale at run time and each container rescans them all. In a CI job it changes nothing.
find /usr/share/fonts /usr/local/share/fonts -type d | while read -r d; do touch -c -d "@$(stat -c %Y "$d")" "$d"; done
fc-cache -f

/usr/lib/libreoffice/program/soffice --version
pdftotext -v 2>&1 | head -1
