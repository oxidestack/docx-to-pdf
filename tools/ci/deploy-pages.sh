#!/bin/sh
# Publishes this run's Pages artifact, as actions/deploy-pages does, under a build version of its own.
#
#   sh tools/ci/deploy-pages.sh <artifact id>
#
# actions/deploy-pages names a deployment by the commit alone (GITHUB_SHA), and Pages answers a second deployment of
# a commit it has already published with that first deployment: the release tag's build, made on the commit `main`
# already deployed, reported success and left the site without the release's chart in the helm repository's index.
# Here the build version is the commit, the run and its attempt, so every run's build is published. The calls are
# deploy-pages': create the deployment with the job's OIDC token, then poll its status (10 s apart, 10 minutes).
# The job needs `pages: write` and `id-token: write`, and GH_TOKEN; page_url goes to GITHUB_OUTPUT.
# Exit 0: published; 1: refused, failed or still pending after 10 minutes; 2: usage.
set -eu

[ $# -eq 1 ] || {
	sed -n '4p' "$0" >&2
	exit 2
}
artifact=$1
repo=$GITHUB_REPOSITORY
version="$GITHUB_SHA.$GITHUB_RUN_ID.$GITHUB_RUN_ATTEMPT"

oidc=$(curl -fsS --retry 3 -H "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" "$ACTIONS_ID_TOKEN_REQUEST_URL" |
	jq -r .value)
echo "::add-mask::$oidc"
# What the deployment names: the artifact's name and run, so a refusal can be told from a wrong artifact.
gh api "repos/$repo/actions/artifacts/$artifact" --jq '"deploy-pages: artifact \(.id) \(.name), run \(.workflow_run.id)"' ||
	echo "deploy-pages: artifact $artifact not found"
# The token travels in the request's body, read from stdin, never as an argument.
answer=$(mktemp)
# The token goes in a header read from a file of the job's own, not in curl's arguments.
headers=$(mktemp)
printf 'Authorization: Bearer %s\n' "$GH_TOKEN" > "$headers"
status=$(jq -n --argjson artifact "$artifact" --arg version "$version" --arg token "$oidc" \
	'{artifact_id: $artifact, pages_build_version: $version, oidc_token: $token}' |
	curl -sS -o "$answer" -w '%{http_code}' -X POST -H @"$headers" \
		-H 'Accept: application/vnd.github+json' -H 'X-GitHub-Api-Version: 2022-11-28' \
		--data-binary @- "${GITHUB_API_URL:-https://api.github.com}/repos/$repo/pages/deployments")
deployment=$(cat "$answer")
rm -f "$answer" "$headers"
case $status in
2??) ;;
*)
	echo "::error title=GitHub Pages::creating the deployment answered HTTP $status: $(printf '%s' "$deployment" | head -c 500)"
	exit 1
	;;
esac
id=$(printf '%s' "$deployment" | jq -r '.id // (.status_url // "" | split("/") | last) // empty')
[ -n "$id" ] || id=$version
echo "deploy-pages: deployment $id of build $version"

tries=60
while [ "$tries" -gt 0 ]; do
	status=$(gh api "repos/$repo/pages/deployments/$id" --jq .status 2> /dev/null || echo unknown)
	case $status in
	succeed)
		url=$(printf '%s' "$deployment" | jq -r '.page_url // empty')
		echo "deploy-pages: published${url:+ at $url}"
		[ -z "$url" ] || [ -z "${GITHUB_OUTPUT:-}" ] || echo "page_url=$url" >> "$GITHUB_OUTPUT"
		exit 0
		;;
	deployment_failed | deployment_content_failed | deployment_cancelled | deployment_lost)
		echo "::error title=GitHub Pages::deployment $id ended as $status"
		exit 1
		;;
	esac
	echo "deploy-pages: $status"
	tries=$((tries - 1))
	sleep 10
done
echo "::error title=GitHub Pages::deployment $id was still not published after 10 minutes"
exit 1
