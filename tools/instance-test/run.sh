#!/bin/sh
# Runs every test, the LibreOfficeKit ones and the converting moalhaddar/docx-to-pdf scenarios included, in a Linux
# container, natively (arm64 on Apple Silicon).
# Usage: tools/instance-test/run.sh   (from anywhere; NAME= names the image and container, KEEP_IMAGE=1 keeps
# the image). Exits with the tests' exit code. The container and, unless kept, the image are removed.
# `$DOCX_TO_PDF_FIXTURES` (a local directory of LibreOffice's DOCX regression documents), when set,
# is mounted read-only at `/fixtures/docx` for the never-ending document (`tdf38575_fullWidthLine.docx`, MPL, kept
# out of the repository); without it that test skips with the reason.
set -u
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
NAME=${NAME:-docx-to-pdf-instance-test}
FIXTURES=${FIXTURES:-${DOCX_TO_PDF_FIXTURES:-}}
docker build --build-context ci="$ROOT/tools/ci" --build-context fonts="$ROOT/upstream/docx-to-pdf/fonts" \
	--build-context image-fontconfig="$ROOT/image/fontconfig" -t "$NAME" "$ROOT/tools/instance-test" || exit $?
if [ -n "$FIXTURES" ] && [ -d "$FIXTURES" ]; then
	docker run --rm --init --name "$NAME" -v "$ROOT":/src:ro -v "$FIXTURES":/fixtures/docx:ro "$NAME" "$@"
else
	docker run --rm --init --name "$NAME" -v "$ROOT":/src:ro "$NAME" "$@"
fi
code=$?
[ "${KEEP_IMAGE:-0}" = 1 ] || docker rmi "$NAME" > /dev/null
echo "instance-test exit=$code"
exit $code
