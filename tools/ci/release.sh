#!/bin/sh
# The release's steps around the image's push (tools/ci/publish-image.sh): what .github/workflows/release.yml checks
# before it builds, the chart it packages, the signatures it makes and checks, the copy to the mirror, the notes of the
# GitHub release and the `helm repo add` index. cosign, oras and helm are the binaries pinned in tools/lint/tools.txt,
# fetched by tools/lint/fetch-tool.sh, so this runs on Linux (a runner, or a container); gh and jq are the runner's.
#
#   sh tools/ci/release.sh check <tag>
#       The tag's version (publish-image.sh's rule) on stdout. For a release (no `-`) the tree must already say it:
#       Chart.yaml's version and appVersion, Cargo.toml's version and a `## [<version>]` section in CHANGELOG.md
#       (tools/release/README.md). A pre-release needs none of them.
#   sh tools/ci/release.sh on-main <owner/repo> <sha>
#       Exit 0 when <sha> is on the default branch (`main` contains it), so a tag on a side branch publishes nothing.
#   sh tools/ci/release.sh wait-ci <owner/repo> <sha> [<minutes>]
#       Waits (default 180 minutes) until a run of ci.yml on <sha> has passed; exit 1 when every run on it failed or
#       the time is up.
#   sh tools/ci/release.sh notes <version> [<image digest>]
#       The GitHub release's body on stdout: CHANGELOG.md's `## [<version>]` section, from the line after the heading
#       to the next `## [`, without its trailing link; a pre-release without a section of its own says what it is.
#       Then how to verify the image and the chart, naming quay.io (and the digest, when given).
#   sh tools/ci/release.sh chart <version> <directory>
#       Packages charts/docx-to-pdf as <directory>/docx-to-pdf-<version>.tgz, its version and appVersion <version>,
#       with the Artifact Hub annotations that depend on it (the image, pre-release); prints the path.
#   sh tools/ci/release.sh quay-login <robot>
#       Exchanges this GitHub Actions job's OIDC token at quay.io for the robot's one-hour token (quay.io's robot
#       federation: no stored secret) and logs in to quay.io. Both tokens are masked in the log, neither is an argument.
#   sh tools/ci/release.sh login <registry> <username>
#       Logs in to <registry> with the password on stdin.
#   sh tools/ci/release.sh sign <reference@digest>
#       Signs with cosign: keyless in CI (the job's OIDC identity, Fulcio and Rekor), or with $COSIGN_KEY.
#   sh tools/ci/release.sh verify <reference> <tag>
#       Checks the cosign signature: made by .github/workflows/release.yml of $GITHUB_REPOSITORY (default
#       oxidestack/docx-to-pdf) at refs/tags/<tag>, issued by GitHub Actions' OIDC; or by $COSIGN_PUB.
#   sh tools/ci/release.sh push-chart <oci repository> <chart.tgz>
#       Pushes the packaged chart (`helm push`); prints its digest.
#   sh tools/ci/release.sh mirror <source repository> <mirror repository> <tag> [<tag>...]
#       Copies <source>:<tag> to the mirror with every referrer it has (cosign's signatures, the SBOM and provenance
#       attestations), so the mirror carries the same digest and the same signed statements; adds the other tags;
#       checks the digests agree; prints the digest.
#   sh tools/ci/release.sh platforms <reference>
#       `<os>/<architecture> <digest>` for each image of the manifest list at <reference>.
#   sh tools/ci/release.sh artifacthub <oci repository>
#       Pushes charts/docx-to-pdf/artifacthub-repo.yml as Artifact Hub's repository metadata (<repository>:artifacthub.io).
#   sh tools/ci/release.sh index <directory> [<releases directory>]
#       Writes <directory>/index.yaml, the classic Helm repository of every chart attached to a GitHub release, each
#       entry downloading from its release. The charts come from `gh release download`, or from <releases
#       directory>/<tag>/*.tgz when given (the tests). No release with a chart: no index, exit 0.
#
# Every registry call is tried three times, 10 s then 30 s apart (RETRY_DELAYS); what fails all three exits 1, and
# whether that fails the release is the workflow's decision (quay.io's does, the mirror's does not).
#
# Environment: DOCKER_CONFIG, the registry credentials every tool here reads (default ~/.docker; crane's CRANE_CONFIG
# should name the same directory); REGISTRY_PLAIN_HTTP=1 for a plain-HTTP test registry; COSIGN_KEY/COSIGN_PUB (and
# COSIGN_PASSWORD) to sign and verify with a key pair instead of keyless, CHANGELOG another changelog (the local tests).
# Exit 0: done; 1: a step failed; 2: usage.
set -eu

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
CHART="$ROOT/charts/docx-to-pdf"
IMAGE_REPOSITORY=quay.io/oxidestack/docx-to-pdf
CHART_REPOSITORY=quay.io/oxidestack/charts/docx-to-pdf
RETRY_DELAYS=${RETRY_DELAYS:-10 30}
AUTH=${DOCKER_CONFIG:-$HOME/.docker}/config.json
CHANGELOG=${CHANGELOG:-$ROOT/CHANGELOG.md}

usage() {
	sed -n '7,46p' "$0" >&2
	exit 2
}

say() { echo "release: $*" >&2; }

# tool <name>: the pinned binary's path.
tool() { sh "$ROOT/tools/lint/fetch-tool.sh" "$1"; }

# retry <what> <command>...: the command up to three times.
retry() {
	what=$1
	shift
	for delay in $RETRY_DELAYS 0; do
		"$@" && return 0
		[ "$delay" = 0 ] && break
		say "$what failed; again in ${delay}s"
		sleep "$delay"
	done
	say "$what failed three times"
	return 1
}

# oras_ <command> <args>: oras with the credentials (and plain HTTP for a test registry); `cp` names both ends.
oras_() {
	if [ "$1" = cp ]; then
		set -- "$@" --from-registry-config "$AUTH" --to-registry-config "$AUTH"
		[ "${REGISTRY_PLAIN_HTTP:-}" != 1 ] || set -- "$@" --from-plain-http --to-plain-http
	else
		set -- "$@" --registry-config "$AUTH"
		[ "${REGISTRY_PLAIN_HTTP:-}" != 1 ] || set -- "$@" --plain-http
	fi
	"$ORAS" "$@"
}

cosign_() {
	# The plain-HTTP flag names cosign's own spelling.
	# shellcheck disable=SC2046 # the flag is one word or none
	DOCKER_CONFIG=$(dirname "$AUTH") "$COSIGN" "$@" $([ "${REGISTRY_PLAIN_HTTP:-}" = 1 ] && echo --allow-http-registry)
}

# yaml_value <file> <key>: a top-level `key: value` of a YAML file, unquoted.
yaml_value() {
	sed -n "s/^$2: *\"\{0,1\}\([^\"]*\)\"\{0,1\} *$/\1/p" "$1" | head -n 1
}

# section <version>: CHANGELOG.md's section for <version>, without the heading and the link at its end.
section() {
	awk -v want="## [$1]" '
		index($0, want) == 1 && (length($0) == length(want) || substr($0, length(want) + 1, 1) == " ") { on = 1; next }
		on && /^## \[/ { exit }
		on { print }
	' "$CHANGELOG" | awk -v v="$1" '
		{ lines[NR] = $0 }
		END {
			n = NR
			# The section ends with its compare link, `[<version>]: https://…`, and blank lines.
			while (n > 0 && (lines[n] ~ /^[ \t]*$/ || index(lines[n], "[" v "]: ") == 1)) n--
			s = 1
			while (s <= n && lines[s] ~ /^[ \t]*$/) s++
			for (i = s; i <= n; i++) print lines[i]
		}
	'
}

[ $# -ge 1 ] || usage
cmd=$1
shift
case $cmd in
check)
	[ $# -eq 1 ] || usage
	version=$(sh "$ROOT/tools/ci/publish-image.sh" version "$1")
	case $version in
	*-*) ;;
	*)
		rc=0
		for key in version appVersion; do
			got=$(yaml_value "$CHART/Chart.yaml" "$key")
			[ "$got" = "$version" ] || {
				say "charts/docx-to-pdf/Chart.yaml's $key is $got, the tag says $version (tools/release/README.md, step 2)"
				rc=1
			}
		done
		got=$(sed -n '/^\[package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' "$ROOT/Cargo.toml" | head -n 1)
		[ "$got" = "$version" ] || {
			say "Cargo.toml's version is $got, the tag says $version (tools/release/README.md, step 2)"
			rc=1
		}
		[ -n "$(section "$version")" ] || {
			say "CHANGELOG.md has no section ## [$version] (tools/release/README.md, steps 1-3)"
			rc=1
		}
		[ $rc -eq 0 ] || exit 1
		;;
	esac
	echo "$version"
	;;
on-main)
	[ $# -eq 2 ] || usage
	status=$(gh api "repos/$1/compare/main...$2" --jq .status)
	case $status in
	identical | behind) ;;
	*)
		say "$2 is not on main (main...$2 is $status): a release is tagged on main"
		exit 1
		;;
	esac
	;;
wait-ci)
	[ $# -ge 2 ] && [ $# -le 3 ] || usage
	repo=$1 sha=$2 minutes=${3:-180}
	deadline=$(($(date +%s) + minutes * 60))
	while :; do
		# Every run of ci.yml on the commit: a push's, a manual one's.
		runs=$(gh api "repos/$repo/actions/workflows/ci.yml/runs?head_sha=$sha&per_page=100" \
			--jq '.workflow_runs[] | "\(.status) \(.conclusion // "-") \(.html_url)"')
		if printf '%s\n' "$runs" | grep -q '^completed success '; then
			say "CI passed on $sha: $(printf '%s\n' "$runs" | grep '^completed success ' | head -n 1 | cut -d' ' -f3)"
			exit 0
		fi
		if [ -n "$runs" ] && ! printf '%s\n' "$runs" | grep -qv '^completed '; then
			say "every CI run on $sha has failed or was cancelled:"
			printf '%s\n' "$runs" >&2
			exit 1
		fi
		[ "$(date +%s)" -lt "$deadline" ] || {
			say "no passed CI run on $sha after $minutes minutes"
			exit 1
		}
		sleep 60
	done
	;;
notes)
	[ $# -ge 1 ] && [ $# -le 2 ] || usage
	version=$1 digest=${2:-}
	body=$(section "$version")
	if [ -z "$body" ]; then
		case $version in
		*-*)
			body="A pre-release of ${version%%-*}, for testing: the changes since the last release are the commits after its tag. \`latest\` stays on the last release."
			;;
		*)
			say "CHANGELOG.md has no section ## [$version]"
			exit 1
			;;
		esac
	fi
	repo=${GITHUB_REPOSITORY:-oxidestack/docx-to-pdf}
	ref="$IMAGE_REPOSITORY:$version"
	identity="https://github.com/$repo/.github/workflows/release.yml@refs/tags/v$version"
	at=""
	[ -z "$digest" ] || at=" (\`$digest\`)"
	printf '%s\n\n' "$body"
	cat << EOF
## Install

\`\`\`sh
docker run --rm -p 8080:8080 $ref
helm install docx-to-pdf oci://$CHART_REPOSITORY --version $version
\`\`\`

The image$at and the chart are published to quay.io and mirrored, with the same digests, signatures and attestations, to \`ghcr.io/oxidestack\`. The SBOMs (SPDX and CycloneDX, one per architecture), the chart and the compose examples are attached below.

## Verify

\`\`\`sh
cosign verify $ref --certificate-identity $identity --certificate-oidc-issuer https://token.actions.githubusercontent.com
gh attestation verify oci://$ref --repo $repo
cosign verify $CHART_REPOSITORY:$version --certificate-identity $identity --certificate-oidc-issuer https://token.actions.githubusercontent.com
\`\`\`
EOF
	;;
chart)
	[ $# -eq 2 ] || usage
	version=$1 out=$2
	HELM=$(tool helm)
	mkdir -p "$out"
	work=$(mktemp -d)
	trap 'rm -rf "$work"' EXIT
	cp -R "$CHART" "$work/docx-to-pdf"
	case $version in
	*-*) prerelease=true ;;
	*) prerelease=false ;;
	esac
	# The annotations that name this version: Artifact Hub's image list (its security scan) and pre-release flag.
	awk -v image="$IMAGE_REPOSITORY:$version" -v pre="$prerelease" '
		{ print }
		/^annotations:/ {
			print "  artifacthub.io/prerelease: \"" pre "\""
			print "  artifacthub.io/images: |"
			print "    - name: docx-to-pdf"
			print "      image: " image
			print "      platforms:"
			print "        - linux/amd64"
			print "        - linux/arm64"
		}
	' "$CHART/Chart.yaml" > "$work/docx-to-pdf/Chart.yaml"
	grep -q '^  artifacthub.io/images:' "$work/docx-to-pdf/Chart.yaml" || {
		say "charts/docx-to-pdf/Chart.yaml has no annotations: block"
		exit 1
	}
	"$HELM" lint "$work/docx-to-pdf" > /dev/null
	"$HELM" package "$work/docx-to-pdf" --version "$version" --app-version "$version" -d "$out" > /dev/null
	echo "$out/docx-to-pdf-$version.tgz"
	;;
quay-login)
	[ $# -eq 1 ] || usage
	robot=$1
	[ -n "$robot" ] || {
		echo "::error title=quay.io::no repository variable QUAY_ROBOT (the robot account, oxidestack+<name>)"
		exit 1
	}
	[ -n "${ACTIONS_ID_TOKEN_REQUEST_URL:-}" ] || {
		say "no OIDC token in this job (it needs id-token: write)"
		exit 1
	}
	oidc=$(curl -fsS --retry 3 -H "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
		"$ACTIONS_ID_TOKEN_REQUEST_URL" | jq -r .value)
	echo "::add-mask::$oidc"
	# curl reads the robot's credentials from its config on stdin, never from an argument.
	token=$(printf 'user = "%s:%s"\n' "$robot" "$oidc" |
		curl -fsS --retry 3 -K - https://quay.io/oauth2/federation/robot/token | jq -r .token)
	[ -n "$token" ] && [ "$token" != null ] || {
		echo "::error title=quay.io::the robot token exchange returned no token"
		exit 1
	}
	echo "::add-mask::$token"
	printf '%s' "$token" | sh "$0" login quay.io "$robot"
	;;
login)
	[ $# -eq 2 ] || usage
	ORAS=$(tool oras)
	mkdir -p "$(dirname "$AUTH")"
	# The password is read from stdin. oras writes a Docker config.json, which cosign, helm, crane and the
	# attestation action read too.
	oras_ login "$1" -u "$2" --password-stdin > /dev/null
	;;
sign)
	[ $# -eq 1 ] || usage
	case $1 in
	*@sha256:*) ;;
	*)
		say "sign a digest, not a tag: $1"
		exit 2
		;;
	esac
	COSIGN=$(tool cosign)
	if [ -n "${COSIGN_KEY:-}" ]; then
		retry "signature of $1" cosign_ sign --yes --key "$COSIGN_KEY" --use-signing-config=false --tlog-upload=false "$1"
	else
		retry "signature of $1" cosign_ sign --yes "$1"
	fi
	;;
verify)
	[ $# -eq 2 ] || usage
	COSIGN=$(tool cosign)
	if [ -n "${COSIGN_PUB:-}" ]; then
		retry "verification of $1" cosign_ verify --key "$COSIGN_PUB" --insecure-ignore-tlog=true "$1" > /dev/null
	else
		repo=${GITHUB_REPOSITORY:-oxidestack/docx-to-pdf}
		retry "verification of $1" cosign_ verify "$1" \
			--certificate-identity "https://github.com/$repo/.github/workflows/release.yml@refs/tags/$2" \
			--certificate-oidc-issuer https://token.actions.githubusercontent.com > /dev/null
	fi
	say "$1: the signature verifies"
	;;
push-chart)
	[ $# -eq 2 ] || usage
	HELM=$(tool helm)
	# shellcheck disable=SC2046 # the flag is one word or none
	out=$(retry "push of $2" "$HELM" push "$2" "oci://${1%/*}" --registry-config "$AUTH" \
		$([ "${REGISTRY_PLAIN_HTTP:-}" = 1 ] && echo --plain-http) 2>&1) || {
		printf '%s\n' "$out" >&2
		exit 1
	}
	digest=$(printf '%s\n' "$out" | sed -n 's/^Digest: *\(sha256:[0-9a-f]*\)$/\1/p' | tail -n 1)
	[ -n "$digest" ] || {
		printf '%s\n' "$out" >&2
		say "helm push printed no digest"
		exit 1
	}
	[ "${1##*/}" = "$(basename "$2" | sed 's/-[0-9][^/]*\.tgz$//')" ] || {
		say "the chart's name is not the repository's: $1, $2"
		exit 1
	}
	echo "$digest"
	;;
mirror)
	[ $# -ge 3 ] || usage
	src=$1 dst=$2 first=$3
	shift 3
	ORAS=$(tool oras)
	# -r: the referrers too, through the OCI referrers API or its tag-schema fallback, whichever each registry has.
	retry "copy to $dst:$first" oras_ cp -r "$src:$first" "$dst:$first" > /dev/null
	for tag in "$@"; do
		retry "tag $dst:$tag" oras_ tag "$dst:$first" "$tag" > /dev/null
	done
	want=$(retry "digest of $src:$first" oras_ resolve "$src:$first")
	got=$(retry "digest of $dst:$first" oras_ resolve "$dst:$first")
	[ "$want" = "$got" ] || {
		say "$dst:$first is $got, $src:$first is $want"
		exit 1
	}
	echo "$got"
	;;
platforms)
	[ $# -eq 1 ] || usage
	ORAS=$(tool oras)
	retry "manifest of $1" oras_ manifest fetch "$1" |
		jq -r '.manifests[] | select(.platform.os != "unknown") | "\(.platform.os)/\(.platform.architecture) \(.digest)"'
	;;
artifacthub)
	[ $# -eq 1 ] || usage
	ORAS=$(tool oras)
	# Artifact Hub's media types (https://artifacthub.io/docs/topics/repositories/helm-charts/); oras wants a path
	# relative to where it runs.
	cd "$CHART"
	retry "Artifact Hub metadata on $1" oras_ push "$1:artifacthub.io" \
		--config /dev/null:application/vnd.cncf.artifacthub.config.v1+yaml \
		artifacthub-repo.yml:application/vnd.cncf.artifacthub.repository-metadata.layer.v1.yaml > /dev/null
	;;
index)
	[ $# -ge 1 ] && [ $# -le 2 ] || usage
	out=$1 releases=${2:-}
	repo=${GITHUB_REPOSITORY:-oxidestack/docx-to-pdf}
	HELM=$(tool helm)
	work=$(mktemp -d)
	trap 'rm -rf "$work"' EXIT
	if [ -z "$releases" ]; then
		releases=$work/releases
		mkdir -p "$releases"
		# Drafts are not published; a release without a chart (none yet) is skipped.
		for tag in $(gh release list -R "$repo" --exclude-drafts --limit 1000 --json tagName --jq '.[].tagName'); do
			gh release download "$tag" -R "$repo" -p 'docx-to-pdf-*.tgz' -D "$releases/$tag" 2> /dev/null || true
		done
	fi
	index=$work/index.yaml
	for dir in "$releases"/*/; do
		[ -d "$dir" ] || continue
		ls "$dir"*.tgz > /dev/null 2>&1 || continue
		tag=$(basename "$dir")
		# Each release's charts are indexed with that release's download URL, merged into one index.
		if [ -f "$index" ]; then
			"$HELM" repo index "$dir" --url "https://github.com/$repo/releases/download/$tag" --merge "$index"
		else
			"$HELM" repo index "$dir" --url "https://github.com/$repo/releases/download/$tag"
		fi
		cp "$dir/index.yaml" "$index"
		rm "$dir/index.yaml"
	done
	if [ -f "$index" ]; then
		mkdir -p "$out"
		cp "$index" "$out/index.yaml"
		say "$out/index.yaml: $(grep -c '^    version:' "$index") chart version(s)"
	else
		say "no GitHub release carries a chart yet: no index"
	fi
	;;
*) usage ;;
esac
