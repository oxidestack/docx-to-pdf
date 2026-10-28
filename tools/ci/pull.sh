#!/bin/sh
# Pulls each image not already present, up to four attempts 2, 4 and 8 s apart, as tools/lint/fetch-tool.sh downloads
# a binary. The CI runner's network drops a large download now and then (curl 56 on get.helm.sh, Trivy's DB "unexpected
# EOF"), and a `docker run`, `kind create cluster` or `docker compose up` pulls its image once, unretried: a script
# pulls through this first. Only the pull is retried; whatever runs the image afterwards runs once.
#
#   sh tools/ci/pull.sh <image>...      exit 0: every image is present; 1: one could not be pulled
set -u
rc=0
for ref in "$@"; do
	attempt=1 delay=2
	until docker image inspect "$ref" > /dev/null 2>&1 || docker pull -q "$ref" > /dev/null; do
		if [ "$attempt" -ge 4 ]; then
			echo "pull: cannot pull $ref ($attempt attempts)" >&2
			rc=1
			break
		fi
		echo "pull: attempt $attempt to pull $ref failed; retrying in ${delay}s" >&2
		sleep "$delay"
		attempt=$((attempt + 1)) delay=$((delay * 2))
	done
done
exit "$rc"
