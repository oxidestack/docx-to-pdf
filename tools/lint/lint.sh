#!/bin/sh
# The quality gates behind `make lint`. Each gate is one command, defined once here
# (`gate_run`), and runs in its tool image pinned by digest in tools/lint/images.Dockerfile (`gate_stage`), so a
# contributor needs Docker and nothing else, and CI runs the same command in the same image.
#
#   sh tools/lint/lint.sh [<gate>[,<gate>]...]   each gate (default: all) in its pinned container   (`make lint [ONLY=]`)
#   sh tools/lint/lint.sh --here <gate>          one gate in place, already inside its image        (`make lint-run GATE=`)
#   sh tools/lint/lint.sh --list                 `<gate> <stage> <image ref>`, one line per gate
#
# Every gate runs even after one fails, and the summary names each failure. Exit 0: every gate passed; 1: one or
# more failed; 2: a usage error, an unknown gate, or no Docker.
#
# A container is `docker run --rm --name docx-to-pdf-lint-<gate>-<pid>`, the working tree mounted at /src. cargo's
# registry and build go to target/lint/ (ignored), apart from the host's target/: the container's build is Linux.
# CI (`--here`): the job's image is the pinned ref (tools/lint/check-pins.rb holds the workflow to it), the workflow sets
# CARGO_HOME, and nothing starts a container.
set -eu

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
IMAGES="$ROOT/tools/lint/images.Dockerfile"

# Gates in the order `make lint` runs them: the quick ones first.
GATES="fmt workflows hadolint public commits typos actions shell ruff yamllint markdownlint links editorconfig gitleaks machete chart-lint chart-kubeconform chart-kube-linter chart-unittest chart-docs deny clippy doc msrv fuzz coverage"

gate_stage() {
	case "$1" in
	fmt | clippy | doc | deny | machete | typos | fuzz | coverage) echo rust ;;
	actions | shell | ruff | links | editorconfig | gitleaks) echo rust ;;
	yamllint) echo chart-testing ;;
	markdownlint) echo node ;;
	msrv) echo msrv ;;
	workflows) echo ruby ;;
	hadolint) echo hadolint ;;
	public | commits) echo python ;;
	chart-*) echo chart-testing ;;
	*) return 1 ;;
	esac
}

# The gate itself, run from the repository root inside the gate's image.
gate_run() {
	case "$1" in
	fmt)
		# The rust image ships without rustfmt and clippy; the toolchain's own components, so they match it.
		rustup -q component add rustfmt
		cargo fmt --check
		;;
	clippy)
		rustup -q component add clippy
		cargo clippy --locked --all-targets -- -D warnings
		;;
	doc)
		# rustdoc's own lints (broken intra-doc links, bare URLs, unclosed HTML) on the crate's documentation, private
		# items included: they are what a contributor reads.
		RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --document-private-items
		;;
	deny)
		# RustSec advisories, licences, bans and sources (deny.toml) on the service's graph and on fuzz/, a workspace
		# of its own that the first run cannot see. The advisory database is fetched each run.
		deny=$(sh tools/lint/fetch-tool.sh cargo-deny)
		rc=0
		"$deny" --locked check || rc=1
		"$deny" --locked --manifest-path fuzz/Cargo.toml check || rc=1
		return $rc
		;;
	machete)
		# A dependency no source file names, in Cargo.toml and fuzz/Cargo.toml.
		"$(sh tools/lint/fetch-tool.sh cargo-machete)"
		;;
	typos)
		# Misspellings in the tracked files (_typos.toml says what is not one).
		"$(sh tools/lint/fetch-tool.sh typos)"
		;;
	msrv)
		# Cargo.toml's rust-version, checked on exactly that toolchain (the msrv stage). The toolchain is asserted
		# first, so a retagged or bumped image cannot pass for it.
		want=$(sed -n 's/^rust-version = "\(.*\)"$/\1/p' Cargo.toml)
		have=$(rustc --version)
		case "$have" in
		"rustc $want "* | "rustc $want.0 "*) echo "msrv: $have, Cargo.toml's rust-version $want" ;;
		*)
			echo "msrv: the msrv stage runs '$have', Cargo.toml's rust-version is '$want'" >&2
			return 1
			;;
		esac
		cargo check --locked --all-targets
		;;
	coverage)
		# Line coverage of the unit and integration tests (cargo llvm-cov), in the `test` job's environment
		# (tools/ci/test-deps.sh: LibreOffice, the fonts, poppler-utils), which the container gets when it lacks it.
		# The report goes to target/lint/coverage/ (html/index.html, lcov.info); below the floor fails.
		command -v pdftotext > /dev/null || sh tools/ci/test-deps.sh full
		rustup -q component add llvm-tools-preview
		PATH="$(dirname "$(sh tools/lint/fetch-tool.sh cargo-llvm-cov)"):$PATH"
		out=target/lint/coverage
		rc=0
		cargo llvm-cov --locked --no-report --no-fail-fast || rc=1
		cargo llvm-cov report --html --output-dir "$out" > /dev/null || rc=1
		cargo llvm-cov report --lcov --output-path "$out/lcov.info" || rc=1
		cargo llvm-cov report --summary-only --fail-under-lines "$COVERAGE_FLOOR" || {
			echo "coverage: lines below the floor of $COVERAGE_FLOOR% (tools/lint/lint.sh COVERAGE_FLOOR)" >&2
			rc=1
		}
		return $rc
		;;
	fuzz)
		# The fuzz targets replay their committed seeds (fuzz/README.md): every file of fuzz/seeds/<target>/
		# runs once through the target binary itself, so the replay is the soak's code and not a copy of it. Stable:
		# libfuzzer-sys builds libFuzzer with the image's C++ compiler, and a binary without coverage instrumentation
		# still runs the inputs it is given; debug assertions and overflow checks on, as cargo-fuzz builds them. No
		# ASan, which needs nightly: that is the soak's, and the downscales are safe Rust. A panic aborts (libfuzzer-sys)
		# and libFuzzer exits non-zero naming the input. A [[bin]] of fuzz/Cargo.toml without seeds fails too.
		dir=${CARGO_TARGET_DIR:-target}/fuzz
		CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=true \
			cargo build --locked --release --manifest-path fuzz/Cargo.toml --bins --target-dir "$dir"
		rc=0
		# shellcheck disable=SC2013 # the [[bin]] names, one word each
		for t in $(awk '/^\[\[bin\]\]/ { bin = 1; next } bin && $1 == "name" { gsub(/"/, "", $3); print $3; bin = 0 }' fuzz/Cargo.toml); do
			set -- fuzz/seeds/"$t"/*
			if [ ! -f "$1" ]; then
				echo "fuzz: the target $t has no seeds in fuzz/seeds/$t/ (fuzz/README.md)" >&2
				rc=1
				continue
			fi
			echo "fuzz: $t, $# seeds"
			"$dir/release/$t" -timeout=10 -rss_limit_mb=2048 "$@" || {
				echo "fuzz: $t failed on a seed above: fuzz/README.md says what to do with it" >&2
				rc=1
			}
		done
		return $rc
		;;
	workflows)
		# GitRiver's rule for every job (vendored, never edited here), then ours: each CI workflow runs these gates in
		# their pinned images through `make lint-run` (tools/lint/check-pins.rb) — GitRiver's, and GitHub's once it
		# exists. And GitRiver's is one workflow: each file there is a pipeline of its own on every push, so a second
		# one splits the push in two and its jobs belong in ci.yml's DAG. All run, so one's
		# finding does not hide another's.
		rc=0
		for f in .gitriver/workflows/*; do
			[ "$f" = .gitriver/workflows/ci.yml ] && continue
			echo "$f: a second GitRiver workflow, a second pipeline per push — its jobs go into ci.yml" >&2
			rc=1
		done
		ruby tools/ci/lint-gitriver-workflows || rc=1
		ruby tools/lint/check-pins.rb .gitriver/workflows/ci.yml || rc=1
		ruby tools/lint/check-pins.rb || rc=1
		ruby tools/lint/test_check_pins.rb || rc=1
		return $rc
		;;
	hadolint)
		# Every Dockerfile that builds something of ours: the service, the tool images, this
		# directory's pins. .hadolint.yaml fails on any finding; an accepted one is an inline ignore with its reason.
		hadolint Dockerfile tools/*/Dockerfile tools/*/*.Dockerfile
		;;
	public)
		# The publication gate's built-in half: local paths and internal hosts. The
		# denylist names private things and is never in a container or a job, so PUBLIC_DENYLIST=none; the whole
		# check is `make public-check` and the pre-push hook. The python image has neither git nor ripgrep. The
		# mounted tree belongs to another user, so git is told it is safe, through the environment rather than a
		# global config a `--here` run on a laptop would keep.
		# shellcheck disable=SC2015 # either one missing installs both
		command -v git > /dev/null && command -v rg > /dev/null || apk add --no-cache -q git ripgrep
		GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0="$PWD" PUBLIC_DENYLIST=none \
			python3 tools/public/public_check.py
		;;
	commits)
		# The commit messages: Conventional Commits in this project's form (tools/lint/commits.py says each rule).
		# The gate's own fixtures first, then every commit after the base: COMMITS_BASE, or commits.py's BASE, which
		# is empty until the public history starts, so until then the fixtures alone and the history before keeps its
		# own style. A pull request's job sets COMMITS_BASE to the pull request's base. COMMITS_SIGNOFF=1 adds the
		# sign-off rule. The python image has no git.
		command -v git > /dev/null || apk add --no-cache -q git
		safe_git python3 tools/lint/commits.py
		;;
	chart-lint)
		# helm lint --strict with the defaults and with every option on, then chart-testing's lint (Chart.yaml's
		# schema, yamllint, maintainers; tools/lint/ct.yaml says which of its checks are off and why). The chart's
		# values.schema.json makes both refuse a misspelt value.
		rc=0
		helm lint --strict "$CHART" || rc=1
		helm lint --strict "$CHART" --values "$ALL_OPTIONS" || rc=1
		ct lint --config tools/lint/ct.yaml || rc=1
		return $rc
		;;
	chart-kubeconform)
		# The rendered manifests against the API schemas of the oldest Kubernetes the chart supports (Chart.yaml's
		# kubeVersion) and the newest released one, strict, so an unknown field fails too. The schemas come from
		# both catalogues at a pinned commit (the ServiceMonitor is a CRD), cached in target/lint/.
		kubeconform=$(sh tools/lint/fetch-tool.sh kubeconform)
		mkdir -p target/lint/kubeconform-cache target/lint/chart
		rc=0
		for k8s in $KUBE_VERSIONS; do
			for set in defaults all-options; do
				echo "kubeconform: Kubernetes $k8s, $set"
				render "$set" "$k8s" > "target/lint/chart/$set-$k8s.yaml" || {
					rc=1
					continue
				}
				"$kubeconform" -strict -summary -kubernetes-version "$k8s" -cache target/lint/kubeconform-cache \
					-schema-location "$K8S_SCHEMAS" -schema-location "$CRD_SCHEMAS" \
					"target/lint/chart/$set-$k8s.yaml" || rc=1
			done
		done
		return $rc
		;;
	chart-kube-linter)
		# Security and reliability practice on the rendered manifests (non-root, read-only root, no privilege
		# escalation, resources, probes, ...): every built-in check, exceptions in tools/lint/kube-linter.yaml.
		kube_linter=$(sh tools/lint/fetch-tool.sh kube-linter)
		newest=${KUBE_VERSIONS##* }
		mkdir -p target/lint/chart
		rc=0
		for set in defaults all-options; do
			render "$set" "$newest" > "target/lint/chart/$set.yaml" || {
				rc=1
				continue
			}
		done
		[ $rc -eq 0 ] || return $rc
		"$kube_linter" lint --config tools/lint/kube-linter.yaml target/lint/chart/defaults.yaml \
			target/lint/chart/all-options.yaml
		;;
	chart-unittest)
		# The templates' logic: charts/docx-to-pdf/tests/*_test.yaml (helm-unittest, its standalone binary).
		"$(sh tools/lint/fetch-tool.sh helm-unittest)" --strict "$CHART"
		;;
	chart-docs)
		# The chart's README.md is helm-docs' output from README.md.gotmpl and values.yaml's `# --` comments; a
		# stale one fails here, and `make chart-docs` writes it.
		mkdir -p target/lint/chart
		"$(sh tools/lint/fetch-tool.sh helm-docs)" --chart-search-root charts --dry-run --log-level warning \
			> target/lint/chart/README.md
		diff -u "$CHART/README.md" target/lint/chart/README.md || {
			echo "chart-docs: $CHART/README.md is stale: make chart-docs" >&2
			return 1
		}
		;;
	actions)
		# GitHub Actions' workflows: actionlint (syntax, expressions, the `run:` scripts through shellcheck)
		# and zizmor (template injection, over-broad `permissions`, unpinned actions, persisted credentials), offline so
		# a push never waits on GitHub's API. Not .gitriver/workflows/: GitRiver's dialect is GitHub's in shape only
		# (`image:`, `timeout:`, jobs without `runs-on:`), both tools report its own keys as errors, and the vendored lint
		# of the `workflows` gate is the one that knows it. Until .github/workflows/ holds a workflow there is nothing
		# to check (.github/'s issue, pull request and discussion templates are no input for either tool, and zizmor
		# fails on "no inputs collected"), which the gate says and passes on; the day one lands, these run on it.
		# Any one file of either extension is a workflow (`ls` of both patterns fails when either matches nothing).
		workflow=""
		for f in .github/workflows/*.yml .github/workflows/*.yaml; do
			[ -f "$f" ] && workflow=$f && break
		done
		if [ -z "$workflow" ]; then
			echo "actions: no workflow in .github/workflows/ yet, nothing for actionlint and zizmor to check (skipped)"
			return 0
		fi
		PATH="$(dirname "$(sh tools/lint/fetch-tool.sh shellcheck)"):$PATH"
		rc=0
		if [ -d .github/workflows ]; then
			"$(sh tools/lint/fetch-tool.sh actionlint)" -no-color || rc=1
		fi
		"$(sh tools/lint/fetch-tool.sh zizmor)" --offline --no-progress --format plain .github || rc=1
		return $rc
		;;
	shell)
		# Every tracked shell script, by extension or by its `#!` line: shellcheck at every severity, then shfmt's
		# layout (.editorconfig carries its options) as a diff.
		# shellcheck disable=SC2046 # one path per line, none with a space (tracked())
		set -- $(shell_files)
		some_files shell $# || return 1
		echo "shell: $# scripts"
		rc=0
		# The scripts are read as UTF-8 whatever the image's locale.
		LC_ALL=C.UTF-8 "$(sh tools/lint/fetch-tool.sh shellcheck)" --external-sources --severity=style "$@" || rc=1
		"$(sh tools/lint/fetch-tool.sh shfmt)" --diff "$@" || rc=1
		return $rc
		;;
	ruff)
		# The tracked Python: ruff's lint, then its format as a check (ruff.toml).
		# shellcheck disable=SC2046 # one path per line, none with a space (tracked())
		set -- $(tracked '*.py')
		some_files ruff $# || return 1
		echo "ruff: $# files"
		ruff=$(sh tools/lint/fetch-tool.sh ruff)
		rc=0
		"$ruff" check --no-cache "$@" || rc=1
		"$ruff" format --no-cache --check --diff "$@" || rc=1
		return $rc
		;;
	yamllint)
		# The tracked YAML, strict (a warning fails too): .yamllint.yaml says what is off and why. The chart's
		# templates are Go templates and not YAML; the chart-lint gate renders and checks them.
		# shellcheck disable=SC2046 # one path per line, none with a space (tracked())
		set -- $(tracked '*.yml' '*.yaml')
		some_files yamllint $# || return 1
		echo "yamllint: $# files"
		yamllint --strict "$@"
		;;
	markdownlint)
		# The Markdown, by markdownlint-cli2 (.markdownlint-cli2.jsonc: the files, the rules and why one is off). npm
		# installs exactly the lockfile's tree, into target/lint/, never into the source tree.
		dir=target/lint/markdownlint
		mkdir -p "$dir"
		cp tools/lint/markdownlint/package.json tools/lint/markdownlint/package-lock.json "$dir/"
		if ! cmp -s tools/lint/markdownlint/package-lock.json "$dir/node_modules/.package-lock.json.src" 2> /dev/null; then
			npm ci --prefix "$dir" --ignore-scripts --no-audit --no-fund --no-update-notifier --cache target/lint/npm-cache --loglevel=error
			cp tools/lint/markdownlint/package-lock.json "$dir/node_modules/.package-lock.json.src"
		fi
		node "$dir/node_modules/markdownlint-cli2/markdownlint-cli2-bin.mjs"
		;;
	links)
		# The links in the tracked Markdown (lychee.toml). On a push, offline: every link to a file of the repository
		# and every #anchor in one must resolve, and no request leaves the job, so a site elsewhere being down never fails
		# a push. LINKS_EXTERNAL=1 (`make lint ONLY=links LINKS_EXTERNAL=1`, and GitHub Actions' schedule once that
		# workflow exists) checks the external ones too, retrying, with a day's cache in target/lint/.
		# shellcheck disable=SC2046 # one path per line, none with a space (tracked())
		set -- $(tracked '*.md')
		some_files links $# || return 1
		echo "links: $# files"
		lychee=$(sh tools/lint/fetch-tool.sh lychee)
		if [ "${LINKS_EXTERNAL:-}" = 1 ]; then
			# lychee keeps its cache in the working directory: target/lint/, never the source tree.
			root=$PWD
			mkdir -p target/lint/lychee
			n=$#
			while [ "$n" -gt 0 ]; do
				set -- "$@" "$root/$1"
				shift
				n=$((n - 1))
			done
			(cd target/lint/lychee && "$lychee" --config "$root/lychee.toml" --root-dir "$root" --cache \
				--max-cache-age 1d --max-retries 4 --retry-wait-time 4 "$@")
		else
			"$lychee" --config lychee.toml --root-dir "$PWD" --offline "$@"
		fi
		;;
	editorconfig)
		# Every tracked text file against .editorconfig (editorconfig-checker): UTF-8, LF, a final newline, no trailing
		# space, tabs or spaces as the file type says. .editorconfig-checker.json (JSON, so the reasons are here):
		# - the indent size is not checked: rustfmt, ruff format and shfmt own the indentation of what they format, and
		#   the checker counts a string's, a docstring's or an argument list's alignment as indentation;
		# - fuzz/seeds/ is fuzz inputs, binary without an extension the checker would know.
		# A block it must not read (Python inside a shell script) is between `editorconfig-checker-disable` and
		# `-enable` comments that say why.
		# shellcheck disable=SC2046 # one path per line, none with a space (tracked())
		set -- $(tracked)
		some_files editorconfig $# || return 1
		echo "editorconfig: $# files"
		"$(sh tools/lint/fetch-tool.sh editorconfig-checker)" -no-color "$@"
		;;
	gitleaks)
		# Credentials (keys, tokens, passwords; gitleaks' rules, .gitleaks.toml), in every commit the clone has — the
		# whole history on a laptop, the pushed commit in CI's shallow clone — and in the working tree as the next
		# commit would take it (tracked and new files). Private names are `make public-check`'s, which
		# gitleaks does not repeat; the two together are what a push must pass.
		safe_git git rev-parse -q --verify HEAD > /dev/null || {
			echo "gitleaks: no commit to scan: not a git checkout" >&2
			return 1
		}
		gitleaks=$(sh tools/lint/fetch-tool.sh gitleaks)
		rc=0
		safe_git "$gitleaks" git --no-banner --no-color --redact . || rc=1
		tree=target/lint/gitleaks-tree
		rm -rf "$tree"
		mkdir -p "$tree"
		# --no-recursion: the submodule's entry is a directory, and its content is not ours.
		safe_git git ls-files -z --cached --others --exclude-standard |
			tar --null --no-recursion --ignore-failed-read -T - -cf - 2> /dev/null | tar -xf - -C "$tree"
		"$gitleaks" dir --no-banner --no-color --redact --config .gitleaks.toml "$tree" || rc=1
		return $rc
		;;
	*) return 2 ;;
	esac
}

# git in a gate's container: the mounted tree belongs to another user, so git is told it is safe, through the
# environment rather than a global config a `--here` run on a laptop would keep.
safe_git() {
	GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0="$PWD" "$@"
}

# The tracked files matching the pathspecs (all of them without one), one per line, the ones deleted in the working
# tree and the submodule left out. No tracked path has a space (the `editorconfig` gate would fail on one first).
tracked() {
	safe_git git ls-files -z -- "$@" | tr '\0' '\n' | while IFS= read -r f; do
		if [ -f "$f" ]; then echo "$f"; fi
	done
}

# A gate whose file list came out empty fails rather than pass on nothing: the tree is then no git checkout with an
# index (a copy, or `make ci-job`'s snapshot), not a repository without such files.
some_files() {
	[ "$2" -gt 0 ] && return 0
	echo "$1: no tracked file to check: is this a git checkout with its index?" >&2
	return 1
}

# The tracked shell scripts: `*.sh`, and a file whose `#!` line names sh, bash or dash.
shell_files() {
	tracked | while IFS= read -r f; do
		case "$f" in
		*.sh) echo "$f" ;;
		*) if head -n 1 "$f" | grep -Eq '^#!.*[/ ](sh|bash|dash)([[:space:]]|$)'; then echo "$f"; fi ;;
		esac
	done
}

# The coverage gate's floor, in percent of lines: the suite measured 93.36% when the gate landed, rounded
# down. Raise it with the suite; never lower it to pass.
COVERAGE_FLOOR=93

# The chart's gates.
CHART=charts/docx-to-pdf
ALL_OPTIONS=$CHART/tests/values/all-options.yaml
# Chart.yaml's kubeVersion floor, and the newest Kubernetes released (a bump moves it).
KUBE_VERSIONS="1.30.0 1.37.1"
K8S_SCHEMAS='https://raw.githubusercontent.com/yannh/kubernetes-json-schema/f7d855e49c34b039c9ef2196444910275e31e09e/{{.NormalizedKubernetesVersion}}-standalone{{.StrictSuffix}}/{{.ResourceKind}}{{.KindSuffix}}.json'
CRD_SCHEMAS='https://raw.githubusercontent.com/datreeio/CRDs-catalog/3eb90d47aba8d61919d4419dd38301536e32a94b/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json'

# The chart rendered with a value set (defaults | all-options) for a Kubernetes version, to stdout.
render() {
	case "$1" in
	defaults) helm template docx-to-pdf "$CHART" --namespace docx-to-pdf --kube-version "$2" ;;
	all-options) helm template docx-to-pdf "$CHART" --namespace docx-to-pdf --kube-version "$2" --values "$ALL_OPTIONS" ;;
	esac
}

image_ref() {
	ref=$(awk -v s="$1" 'toupper($1) == "FROM" && toupper($3) == "AS" && $4 == s { print $2 }' "$IMAGES")
	case "$ref" in
	*@sha256:*) echo "$ref" ;;
	*)
		echo "lint: stage '$1' in $IMAGES is missing or not pinned by digest" >&2
		return 2
		;;
	esac
}

usage() {
	echo "usage: sh tools/lint/lint.sh [<gate>[,<gate>]...] | --here <gate> | --list   (gates: $GATES)" >&2
	exit 2
}

case "${1:-}" in
--list)
	for g in $GATES; do
		s=$(gate_stage "$g")
		echo "$g $s $(image_ref "$s")"
	done
	exit 0
	;;
--here)
	[ $# -eq 2 ] || usage
	gate_stage "$2" > /dev/null || usage
	cd "$ROOT"
	gate_run "$2"
	exit $?
	;;
-*) usage ;;
esac

selected=$(echo "${*:-$GATES}" | tr ',' ' ')
for g in $selected; do gate_stage "$g" > /dev/null || {
	echo "lint: no gate '$g'" >&2
	usage
}; done
command -v docker > /dev/null || {
	echo "lint: needs docker (or run one gate in place: make lint-run GATE=<gate>)" >&2
	exit 2
}

running=""
trap '[ -z "$running" ] || docker rm -f -v "$running" > /dev/null 2>&1; exit 130' INT TERM
mkdir -p "$ROOT/target/lint"
failed=""
for g in $selected; do
	ref=$(image_ref "$(gate_stage "$g")")
	running="docx-to-pdf-lint-$g-$$"
	echo "--- lint: $g in $ref"
	set +e
	docker run --rm --init --name "$running" -v "$ROOT:/src" -w /src \
		-e CARGO_HOME=/src/target/lint/cargo -e CARGO_TARGET_DIR=/src/target/lint/target -e CARGO_TERM_COLOR -e LINKS_EXTERNAL \
		-e COMMITS_BASE -e COMMITS_SIGNOFF \
		"$ref" sh tools/lint/lint.sh --here "$g"
	code=$?
	set -e
	running=""
	if [ "$code" -eq 0 ]; then
		echo "--- lint: $g passed"
	else
		echo "--- lint: $g FAILED (exit $code)"
		failed="$failed $g"
	fi
done

if [ -n "$failed" ]; then
	echo "lint: failed:$failed"
	exit 1
fi
echo "lint: passed: $selected"
