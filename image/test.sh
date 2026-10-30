#!/bin/sh
# shellcheck disable=SC2015 # `test && ok … || fail …` is an if-then-else here: ok() only echoes and cannot fail.
# Checks the service's image from outside, as a deployment runs it.
# Usage: image/test.sh [IMAGE]   (default docx-to-pdf:dev; `make image-test`). Exits non-zero on any failure.
#
# Environment:
#   NAME=docx-to-pdf-image-test   prefix of every container it starts (each is removed, with its volumes)
#   OUT=<dir>                      where inventories, PDFs and measurements go (default target/image-test). It
#                                  must be shared with the Docker VM (a macOS Docker VM typically shares $HOME, not /tmp): the
#                                  mounted-font test bind-mounts a directory from it. An inventory of the other architecture already in OUT is compared with this one.
#   CJK_DOC=<path>                 a Chinese document that must embed a Noto CJK font (default the corpus's
#                                  tests/corpus/documents/features/chinese-spec.docx)
#   READY_TIMEOUT=<s>              seconds to wait for /readyz (default 300: an emulated amd64 starts slowly)
#
# Every service container runs under the chart's security context (charts/docx-to-pdf): uid 10001 with no passwd
# entry, a read-only root filesystem, only /tmp writable, no extra capabilities.
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd)
IMAGE=${1:-docx-to-pdf:dev}
NAME=${NAME:-docx-to-pdf-image-test}
OUT=${OUT:-$ROOT/target/image-test}
CJK_DOC=${CJK_DOC:-$ROOT/tests/corpus/documents/features/chinese-spec.docx}
READY_TIMEOUT=${READY_TIMEOUT:-300}
# A free font that is not in the image (OFL-1.1), from the image's own snapshot, pinned by hash.
LOBSTER_URL=https://snapshot.debian.org/archive/debian/20261026T202550Z/pool/main/f/fonts-lobstertwo/fonts-lobster_2.0-3_all.deb
LOBSTER_SHA256=e7874c5504678ca13675c684878189baa7e5396d2cc40a933da4fd326e30ebd6
mkdir -p "$OUT" && OUT=$(cd "$OUT" && pwd)

FAILED=0
ok() { echo "ok      $*"; }
fail() {
	echo "FAIL    $*"
	FAILED=1
}
measure() {
	echo "measure $*"
	echo "$*" >> "$OUT/measurements-$ARCH.txt"
}

# The host's tools: without them a check fails as if the image were at fault (no `ar`, no `pdffonts` on a fresh
# Ubuntu VM once read as "the mounted font is not embedded"). binutils, poppler-utils.
for tool in ar pdffonts python3 curl tar xz; do
	command -v "$tool" > /dev/null || {
		echo "the host lacks $tool (binutils, poppler-utils, python3, curl, tar, xz-utils)"
		exit 2
	}
done

ARCH=$(docker image inspect -f '{{.Architecture}}' "$IMAGE") || {
	echo "no image $IMAGE"
	exit 2
}
PLATFORM=linux/$ARCH
: > "$OUT/measurements-$ARCH.txt"
echo "image $IMAGE ($PLATFORM), output in $OUT"

# Every container this run starts is named $NAME-<what>-$$; at exit each is removed with its anonymous volumes.
# shellcheck disable=SC2329 # run by the trap below
cleanup() {
	for c in $(docker ps -aq --filter "name=^$NAME-.*-$$\$"); do docker rm -f -v "$c" > /dev/null 2>&1; done
}
trap cleanup EXIT INT TERM

# A shell in the image, as the image's user, nothing served.
in_image() { docker run --rm --platform "$PLATFORM" --name "$NAME-sh-$$" --entrypoint sh "$IMAGE" -c "$1"; }

# --- the LibreOffice version and the font set, per architecture -----------------------------------------------
# shellcheck disable=SC2016 # the script runs in the image, which expands it there
in_image '
	echo "user $(id -u):$(id -g)"
	dpkg-query -W -f "\${Package} \${Version}\n" libreoffice-core-nogui libreoffice-writer-nogui libreoffice-math-nogui libreoffice-calc-nogui libreoffice-uiconfig-writer fonts-opensymbol
	dpkg-query -W -f "\${Package} \${Version}\n" | grep "^fonts-" | sort
	fc-list : family | sort -u | sha256sum | sed "s/ .*//; s/^/families-sha256 /"
	echo "families $(fc-list : family | sort -u | wc -l)"
	echo "font-files $(fc-list : file | wc -l)"
' > "$OUT/inventory-$ARCH.txt" 2>&1 || fail "inventory: $(tail -1 "$OUT/inventory-$ARCH.txt")"
sed -n '1,7p;/^families/p;/^font-files/p' "$OUT/inventory-$ARCH.txt" | sed 's/^/        /'
grep -q '^user 10001:10001$' "$OUT/inventory-$ARCH.txt" && ok "runs as uid 10001" || fail "the image's user is not 10001:10001"
grep -q '^libreoffice-core-nogui 4:26\.8\.' "$OUT/inventory-$ARCH.txt" && ok "LibreOffice 26.8" ||
	fail "libreoffice-core-nogui is not 26.8"
# Without it a LOK load of any document with a comment fails.
grep -q '^libreoffice-uiconfig-writer ' "$OUT/inventory-$ARCH.txt" && ok "libreoffice-uiconfig-writer installed" ||
	fail "libreoffice-uiconfig-writer is missing (documents with comments do not load)"
# Without it an embedded spreadsheet draws nothing, neither its cells nor its preview picture.
grep -q '^libreoffice-calc-nogui ' "$OUT/inventory-$ARCH.txt" && ok "libreoffice-calc-nogui installed" ||
	fail "libreoffice-calc-nogui is missing (an embedded spreadsheet draws nothing)"
# Without it, parallel first starts as root race to create it and one exits 1.
in_image 'test -d /var/spool/libreoffice/uno_packages/cache/uno_packages' > /dev/null 2>&1 &&
	ok "the shared extension folder exists" ||
	fail "/var/spool/libreoffice/uno_packages/cache/uno_packages is missing (parallel first starts as root race)"
for other in "$OUT"/inventory-*.txt; do
	[ "$other" = "$OUT/inventory-$ARCH.txt" ] && continue
	# The first line is the user, the same on both; everything after it must be identical.
	if [ "$(sed 1d "$other")" = "$(sed 1d "$OUT/inventory-$ARCH.txt")" ]; then
		ok "same LibreOffice and font set as $(basename "$other")"
	else
		fail "LibreOffice or fonts differ from $(basename "$other")"
	fi
done

# --- fontconfig: the common Word font names resolve as in moalhaddar/docx-to-pdf -----------------------------------
names=$(grep -v '^#' "$ROOT/image/fc-match.expected" | cut -f1 | tr '\n' '|')
in_image "
	IFS='|'
	for n in \$(printf '%s' '$names'); do printf '%s\t%s\n' \"\$n\" \"\$(fc-match -f '%{family}' \"\$n\")\"; done
" > "$OUT/fc-match-$ARCH.txt"
mismatches=0
while IFS="$(printf '\t')" read -r name want; do
	case "$name" in '#'* | '') continue ;; esac
	got=$(awk -F '\t' -v n="$name" '$1 == n { print $2 }' "$OUT/fc-match-$ARCH.txt")
	# fc-match prints every family name of the face (e.g. "Cantarell,Cantarell ExtraBold"); the wanted one is among them.
	case ",$got," in *",$want,"*) ;; *)
		echo "        $name: want $want, got $got"
		mismatches=$((mismatches + 1))
		;;
	esac
done < "$ROOT/image/fc-match.expected"
[ $mismatches = 0 ] && ok "fc-match equals moalhaddar/docx-to-pdf's for the common Word font names" ||
	fail "fc-match differs from moalhaddar/docx-to-pdf's for $mismatches names"

# --- the font directories ----------------------------------------------------------------------------------------
# shellcheck disable=SC2016 # the script runs in the image, which expands it there
dirs=$(in_image 'echo "custom:$(ls -A /usr/share/fonts/custom | tr "\n" " ")"; echo "ours:$(ls -A /usr/share/fonts/docx-to-pdf | tr "\n" " ")"')
echo "$dirs" | grep -qx 'custom:' && ok "/usr/share/fonts/custom exists and is empty" ||
	fail "/usr/share/fonts/custom: $(echo "$dirs" | grep '^custom')"
echo "$dirs" | grep -qx 'ours:Cantarell-ExtraBold.otf DroidKufi-Bold.ttf DroidKufi-Regular.ttf ' &&
	ok "upstream's three files, and only they, are in /usr/share/fonts/docx-to-pdf" ||
	fail "/usr/share/fonts/docx-to-pdf: $(echo "$dirs" | grep '^ours')"

# --- the service ---------------------------------------------------------------------------------------------------
# start <suffix> <docker run options...>: runs the service under the chart's security context; prints the port.
start() {
	suffix=$1
	shift
	c="$NAME-$suffix-$$"
	docker run -d --platform "$PLATFORM" --name "$c" --user 10001:10001 --read-only --cap-drop ALL \
		--security-opt no-new-privileges -p 127.0.0.1::8080 "$@" "$IMAGE" > /dev/null || return 1
	docker port "$c" 8080/tcp | head -1 | sed 's/.*://'
}
# wait_ready <port>: prints the seconds from now until /readyz answers 200.
wait_ready() {
	since=$(date +%s)
	until [ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$1/readyz")" = 200 ]; do
		[ $(($(date +%s) - since)) -gt "$READY_TIMEOUT" ] && return 1
		sleep 0.1
	done
}
# convert <port> <document> <pdf>: POST /pdf; succeeds on a 200 with a PDF.
convert() {
	code=$(curl -s -o "$3" -w '%{http_code}' -F "document=@$2" "http://127.0.0.1:$1/pdf")
	[ "$code" = 200 ] && head -c 5 "$3" | grep -q '%PDF-'
}
ms_now() { python3 -c 'import time; print(int(time.time() * 1000))'; }

# /tmp on disk (the default): an anonymous volume, initialised from the image's /tmp (mode 1777).
t0=$(ms_now)
port=$(start disk --mount type=volume,dst=/tmp -e DOCX_TO_PDF_POOL_SIZE=1)
if [ -n "$port" ] && wait_ready "$port" > /dev/null; then
	measure "ready-ms disk-tmp pool=1 $(($(ms_now) - t0))"
	[ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port/livez")" = 200 ] && ok "/livez answers 200" ||
		fail "/livez"
	all=1
	for ex in ex1 ex2 ex3 ex4; do
		convert "$port" "$ROOT/upstream/docx-to-pdf/examples/$ex/input.docx" "$OUT/$ex-$ARCH.pdf" || {
			all=0
			fail "converting $ex"
		}
	done
	[ $all = 1 ] && ok "upstream's four examples convert (uid 10001, read-only root, /tmp on disk)"
	# Chinese text needs the image's Noto CJK fonts: without them LibreOffice substitutes a font with no CJK glyphs.
	if convert "$port" "$CJK_DOC" "$OUT/cjk-$ARCH.pdf"; then
		fonts=$(pdffonts "$OUT/cjk-$ARCH.pdf" 2> /dev/null | awk 'NR > 2 { print $1 }' | sed 's/^[A-Z]\{6\}+//' | sort -u | tr '\n' ' ')
		echo "        $(basename "$CJK_DOC") embeds: $fonts"
		echo "$fonts" | grep -q 'NotoS[a-z]*CJK' && ok "the CJK document embeds a Noto CJK font" ||
			fail "the CJK document embeds no Noto CJK font"
	else
		fail "converting $(basename "$CJK_DOC")"
	fi
	# SIGTERM reaches the binary through tini: the container stops by itself, well before Docker's 10 s SIGKILL.
	t0=$(ms_now)
	docker stop -t 30 "$NAME-disk-$$" > /dev/null
	stop_ms=$(($(ms_now) - t0))
	exit_code=$(docker inspect -f '{{.State.ExitCode}}' "$NAME-disk-$$")
	[ "$exit_code" = 0 ] && [ $stop_ms -lt 25000 ] && ok "SIGTERM stops the service cleanly (exit 0 in ${stop_ms} ms)" ||
		fail "SIGTERM: exit $exit_code after ${stop_ms} ms"
else
	fail "the service did not become ready with /tmp on disk"
	docker logs "$NAME-disk-$$" 2>&1 | tail -20
fi

# /tmp on tmpfs (the opt-in "no disk" mode): what a conversion costs in RAM there.
port=$(start tmpfs --tmpfs /tmp:rw,mode=1777,size=1g -e DOCX_TO_PDF_POOL_SIZE=1)
c="$NAME-tmpfs-$$"
if [ -n "$port" ] && wait_ready "$port" > /dev/null; then
	used() { docker exec "$c" sh -c 'df -k /tmp | awk "NR == 2 { print \$3 }"'; }
	base=$(used)
	measure "tmpfs-kib after-ready (profile, fontconfig cache) $base"
	all=1
	for doc in "$ROOT/upstream/docx-to-pdf/examples/ex1/input.docx" "$ROOT/upstream/docx-to-pdf/examples/ex4/input.docx" \
		"$CJK_DOC"; do
		label=$(basename "$(dirname "$doc")")-$(basename "$doc")
		docker exec "$c" sh -c 'while :; do df -k /tmp | awk "NR == 2 { print \$3 }"; sleep 0.02; done' > "$OUT/tmpfs-samples.txt" 2> /dev/null &
		sampler=$!
		sleep 0.5
		convert "$port" "$doc" "$OUT/tmpfs-$ARCH.pdf" || {
			all=0
			fail "converting $label on tmpfs"
		}
		sleep 0.3
		kill $sampler 2> /dev/null
		wait $sampler 2> /dev/null
		peak=$(sort -n "$OUT/tmpfs-samples.txt" | tail -1)
		after=$(used)
		doc_kib=$(($(wc -c < "$doc") / 1024))
		pdf_kib=$(($(wc -c < "$OUT/tmpfs-$ARCH.pdf") / 1024))
		measure "tmpfs-kib $label document=$doc_kib pdf=$pdf_kib peak-above-before=$((${peak:-0} - base)) left-after=$((after - base))"
		base=$after
	done
	[ $all = 1 ] && ok "converts with /tmp on tmpfs (uid 10001, read-only root)"
else
	fail "the service did not become ready with /tmp on tmpfs"
	docker logs "$c" 2>&1 | tail -20
fi
docker rm -f -v "$c" > /dev/null 2>&1

# --- a font from outside: a read-only bind mount on /usr/share/fonts/custom -----------------------------------------
fonts_dir="$OUT/custom-fonts"
if [ ! -f "$fonts_dir/lobster.otf" ]; then
	mkdir -p "$fonts_dir" "$OUT/lobster"
	curl -sSLf -m 120 --retry 3 --retry-all-errors --retry-delay 2 -o "$OUT/lobster/lobster.deb" "$LOBSTER_URL" &&
		[ "$(shasum -a 256 "$OUT/lobster/lobster.deb" | cut -d' ' -f1)" = "$LOBSTER_SHA256" ] &&
		(cd "$OUT/lobster" && ar x lobster.deb data.tar.xz && tar -xJf data.tar.xz ./usr/share/fonts/opentype/lobster/lobster.otf) &&
		cp "$OUT/lobster/usr/share/fonts/opentype/lobster/lobster.otf" "$fonts_dir/"
fi
if [ -f "$fonts_dir/lobster.otf" ]; then
	family=$(docker run --rm --platform "$PLATFORM" --name "$NAME-scan-$$" -v "$fonts_dir:/f:ro" --entrypoint fc-scan "$IMAGE" \
		-f '%{family[0]}' /f/lobster.otf)
	[ -n "$family" ] || fail "fc-scan found no font in $fonts_dir (is it shared with the Docker VM?)"
	# A document set in that family only.
	# editorconfig-checker-disable: the Python below is indented with spaces, as Python is
	python3 - "$OUT/mounted-font.docx" "$family" << 'PY'
import sys, zipfile
path, family = sys.argv[1], sys.argv[2]
W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
parts = {
  "[Content_Types].xml": '<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>',
  "_rels/.rels": '<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>',
  "word/document.xml": f'<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="{W}"><w:body><w:p><w:r><w:rPr><w:rFonts w:ascii="{family}" w:hAnsi="{family}" w:cs="{family}"/><w:sz w:val="48"/></w:rPr><w:t>A font mounted from outside the image.</w:t></w:r></w:p></w:body></w:document>',
}
with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
    for name, text in parts.items():
        z.writestr(name, text)
PY
	# editorconfig-checker-enable
	# The instance start time with and without the mounted font: the first scan writes fontconfig's cache to /tmp/.cache.
	for run in without with; do
		if [ $run = with ]; then mount="-v $fonts_dir:/usr/share/fonts/custom:ro"; else mount=""; fi
		t0=$(ms_now)
		# shellcheck disable=SC2086
		port=$(start "font-$run" --mount type=volume,dst=/tmp -e DOCX_TO_PDF_POOL_SIZE=1 $mount)
		if [ -n "$port" ] && wait_ready "$port" > /dev/null; then
			measure "ready-ms custom-font=$run pool=1 $(($(ms_now) - t0))"
			# The image's own caches are valid at run time (the Dockerfile's mtime rounding), so fontconfig writes under
			# /tmp/.cache only LibreOffice's empty application font directories (~130 bytes each) and the mounted one.
			cache=$(docker exec "$NAME-font-$run-$$" sh -c 'cat /tmp/.cache/fontconfig/*cache-* 2>/dev/null | wc -c')
			measure "fontconfig-cache-bytes custom-font=$run $cache"
			if [ $run = without ]; then
				nomount_cache=$cache
				[ "$cache" -lt 4096 ] && ok "the image's fontconfig caches are valid at run time ($cache bytes under /tmp/.cache)" ||
					fail "fontconfig rewrote the image's caches under /tmp/.cache ($cache bytes) with no font mounted"
			else
				[ "$cache" -gt "${nomount_cache:-0}" ] && [ "$cache" -lt 65536 ] &&
					ok "fontconfig cached only the mounted directory under /tmp/.cache ($cache bytes)" ||
					fail "fontconfig wrote $cache bytes under /tmp/.cache with one font mounted"
				if convert "$port" "$OUT/mounted-font.docx" "$OUT/mounted-font-$ARCH.pdf"; then
					fonts=$(pdffonts "$OUT/mounted-font-$ARCH.pdf" 2> /dev/null | awk 'NR > 2 { print $1 }' | sed 's/^[A-Z]\{6\}+//' | sort -u | tr '\n' ' ')
					echo "        mounted-font.docx ($family) embeds: $fonts"
					[ -n "$family" ] && echo "$fonts" | grep -qi "$(echo "$family" | tr -d ' ')" && ok "a font mounted read-only into /usr/share/fonts/custom is embedded" ||
						fail "the mounted font $family is not embedded"
				else
					fail "converting the document set in the mounted font"
				fi
			fi
		else
			fail "the service did not become ready (custom font $run)"
			docker logs "$NAME-font-$run-$$" 2>&1 | tail -20
		fi
		docker rm -f -v "$NAME-font-$run-$$" > /dev/null 2>&1
	done
else
	fail "could not fetch the free font for the mount test ($LOBSTER_URL)"
fi

[ $FAILED = 0 ] && echo "image-test passed ($PLATFORM)" || echo "image-test FAILED ($PLATFORM)"
exit $FAILED
