#!/bin/sh
# Fetches one release binary pinned in tools/lint/tools.txt for this Linux machine's architecture, checks its sha256,
# and prints the path of the executable. A binary already fetched at that version is reused.
#
#   sh tools/lint/fetch-tool.sh <tool>      e.g. PATH="$(dirname "$(sh tools/lint/fetch-tool.sh kubeconform)"):$PATH"
#
# The binary lands in target/lint/bin/<arch>/<tool>-<version>/<tool>. Needs curl or wget, sha256sum and tar.
# Exit 0: the path is printed; 1: the download or its hash failed; 2: no such tool, or not Linux on arm64/amd64.
set -eu

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
PINS="$ROOT/tools/lint/tools.txt"
[ $# -eq 1 ] || {
	echo "usage: sh tools/lint/fetch-tool.sh <tool>" >&2
	exit 2
}
tool=$1
[ "$(uname -s)" = Linux ] || {
	echo "fetch-tool: Linux only (run it in a container)" >&2
	exit 2
}
case "$(uname -m)" in
aarch64 | arm64) arch=arm64 ;;
x86_64 | amd64) arch=amd64 ;;
*)
	echo "fetch-tool: no pin for $(uname -m)" >&2
	exit 2
	;;
esac

line=$(awk -v t="$tool" -v a="$arch" '$1 == t && $3 == a { print $2, $4, $5; exit }' "$PINS")
[ -n "$line" ] || {
	echo "fetch-tool: no '$tool' for $arch in $PINS" >&2
	exit 2
}
read -r version sum url << EOF
$line
EOF
dir="$ROOT/target/lint/bin/$arch/$tool-$version"
bin="$dir/$tool"
if [ -x "$bin" ]; then
	echo "$bin"
	exit 0
fi

tmp="$dir.part.$$"
rm -rf "$tmp"
mkdir -p "$tmp"
trap 'rm -rf "$tmp"' EXIT
asset="$tmp/asset"
# One dropped connection must not fail a job (curl 56 on get.helm.sh, pipeline 16438): up to four attempts, 2, 4 and
# 8 s apart. The loop is ours, not curl's --retry, which skips such errors without --retry-all-errors, and busybox
# wget has neither. Only the download is retried; a hash mismatch below fails at once.
download() {
	attempt=1 delay=2
	while :; do
		rm -f "$asset"
		if command -v curl > /dev/null; then
			curl -fsSL -o "$asset" "$url" && return 0
		else
			wget -q -O "$asset" "$url" && return 0
		fi
		[ "$attempt" -lt 4 ] || return 1
		echo "fetch-tool: attempt $attempt to download $url failed; retrying in ${delay}s" >&2
		sleep "$delay"
		attempt=$((attempt + 1)) delay=$((delay * 2))
	done
}
download || {
	echo "fetch-tool: cannot download $url" >&2
	exit 1
}
# The hash is compared here, not by `sha256sum -c`: its quiet flag differs between GNU coreutils (--status only),
# busybox (-s) and macOS (neither), and a checker that fails must not read as a wrong pin.
got=$(sha256sum "$asset") || {
	echo "fetch-tool: sha256sum failed on $url" >&2
	exit 1
}
got=${got%% *}
[ "$got" = "$sum" ] || {
	echo "fetch-tool: $url does not match the sha256 pinned in tools/lint/tools.txt (pinned $sum, got $got)" >&2
	exit 1
}
case "$url" in
*.tar.gz | *.tgz)
	tar -xzf "$asset" -C "$tmp"
	rm "$asset"
	# helm's archive holds linux-<arch>/helm; helm-unittest's, untt-linux-<arch> beside its plugin.yaml.
	found=$(find "$tmp" -type f \( -name "$tool" -o -name "untt-linux-$arch" \) | head -1)
	[ -n "$found" ] || {
		echo "fetch-tool: no $tool in $url" >&2
		exit 1
	}
	mv "$found" "$tmp/$tool.bin"
	find "$tmp" -mindepth 1 ! -name "$tool.bin" -exec rm -rf {} + 2> /dev/null || true
	mv "$tmp/$tool.bin" "$tmp/$tool"
	;;
*) mv "$asset" "$tmp/$tool" ;;
esac
chmod 0755 "$tmp/$tool"
rm -rf "$dir"
mv "$tmp" "$dir"
trap - EXIT
echo "$bin"
