# The tests of tools/ (Python stdlib).
PY ?= python3

.PHONY: tools-test

tools-test: ## the tests of tools/
	$(PY) -m unittest discover -s tools/tests
	$(PY) -m unittest discover -s tools/public
	$(PY) -m unittest discover -s tools/lint
	$(PY) -m unittest discover -s tools/release
	$(PY) -m unittest discover -s tools/compare

# The release notes (tools/release/README.md): the next release's section of CHANGELOG.md drafted from the commits'
# release-note blocks since the last tag, by the pinned git-cliff (cliff.toml), in its container. A human edits it.
.PHONY: release-notes
release-notes: ## draft the next release's section at the top of CHANGELOG.md; REPO=<dir> drafts another repository's
	sh tools/release/release-notes.sh $(REPO)

# A bare `make` stays `cargo test`.
.DEFAULT_GOAL := test

.PHONY: test check oracle oracle-wait oracle-stop record

test: ## every Rust test (`cargo test`)
	cargo test

check: ## formatting, clippy (warnings are errors) and the library tests
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test --lib

# The quality gates (tools/lint/lint.sh), one command locally and exactly the same in CI: each in its tool image
# pinned by digest in tools/lint/images.Dockerfile, so only Docker is needed. CI runs each gate as its own
# `lint-<gate>` job in that same image through `make lint-run`, and tools/lint/check-pins.rb (the `workflows` gate)
# holds it to that.
.PHONY: lint lint-run commits-check

lint: ## every quality gate, each in its pinned tool container; ONLY=<gate>[,<gate>] runs some (`sh tools/lint/lint.sh --list`)
	sh tools/lint/lint.sh $(ONLY)

lint-run: ## one gate in place, inside its image (what the CI jobs call): GATE=<gate>
	sh tools/lint/lint.sh --here $(GATE)

# The `commits` gate's half for a push: the pre-push hook calls it with PUSH=<remote> and git's ref lines on stdin, and
# it checks the pushed commits after the base (tools/lint/commits.py). On the host, as the hook runs: python3 and git.
commits-check: ## the messages of the commits a push sends, after the base: PUSH=<remote>, git's pre-push ref lines on stdin
	$(PY) tools/lint/commits.py --pre-push $(PUSH)

# The chart's README is helm-docs' output: the `chart-docs` gate fails on a stale one, and this writes it,
# in the chart gates' pinned image.
.PHONY: chart-docs
chart-docs: ## regenerate charts/docx-to-pdf/README.md from README.md.gotmpl and values.yaml's `# --` comments
	docker run --rm --name docx-to-pdf-chart-docs-$$$$ -v "$(CURDIR):/src" -w /src \
		$$(sh tools/lint/lint.sh --list | awk '$$1 == "chart-docs" { print $$3 }') \
		sh -c '"$$(sh tools/lint/fetch-tool.sh helm-docs)" --chart-search-root charts --log-level warning'

# The README's headline numbers, rewritten from the committed results of the latest full `make compare` run
# (site/public/benchmarks/results.json) by site/scripts/evidence.mjs in the same pinned node image; `make site` fails
# on a README whose block differs from what this writes.
.PHONY: evidence
evidence: ## rewrite the README's and the comparison guide's blocks from site/public/benchmarks/results.json
	docker run --rm --name docx-to-pdf-evidence-$$$$ --user "$$(id -u):$$(id -g)" -e HOME=/tmp -v "$(CURDIR):/src" -w /src \
		$$(sh tools/lint/lint.sh --list | awk '$$1 == "markdownlint" { print $$3 }') node site/scripts/evidence.mjs --write-readme

# The documentation site (site/README.md): README.md and docs/guide/ built with Astro Starlight into site/dist/, a
# broken internal link or anchor failing the build. Node runs only in the `markdownlint` gate's pinned node image
# (tools/lint/images.Dockerfile), never on the host; the CI's `site` job runs site/build.sh in that same image.
.PHONY: site
site: ## build the documentation site into site/dist/ in the pinned node image; a broken internal link fails it
	docker run --rm --name docx-to-pdf-site-$$$$ --user "$$(id -u):$$(id -g)" -e HOME=/tmp -v "$(CURDIR):/src" -w /src \
		$$(sh tools/lint/lint.sh --list | awk '$$1 == "markdownlint" { print $$3 }') sh site/build.sh

# The publication gate (tools/public/public_check.py): no company's name, nothing of the author's environment, no
# internal host and no local path in a pushed tree or commit message. The denylist names the private things and so
# lives outside the repository: PUBLIC_DENYLIST, default ~/.config/oxidestack/public-denylist (one per organisation,
# its projects share the private names; the organisation's former directory is read when that one is missing);
# PUBLIC_DENYLIST=none runs the built-in local-path and internal-host patterns alone. The pre-push hook calls it with
# PUSH=<remote> and git's ref lines on stdin.
.PHONY: public-check hooks
HOOKS_DIR ?= $(shell git rev-parse --git-path hooks)

public-check: ## fail on a private name, a local path or an internal host in a tracked file or an unpushed commit message; PUBLIC_DENYLIST=<file>|none
	$(if $(PUBLIC_DENYLIST),PUBLIC_DENYLIST='$(PUBLIC_DENYLIST)') $(PY) tools/public/public_check.py $(if $(PUSH),--pre-push $(PUSH))

hooks: ## install the pre-push hook: make check, make public-check and make commits-check before every push
	@mkdir -p '$(HOOKS_DIR)'
	@if [ -e '$(HOOKS_DIR)/pre-push' ] && ! cmp -s tools/public/pre-push '$(HOOKS_DIR)/pre-push'; then \
		echo "hooks: $(HOOKS_DIR)/pre-push exists and differs from tools/public/pre-push; move it away first" >&2; exit 1; fi
	install -m 755 tools/public/pre-push '$(HOOKS_DIR)/pre-push'

# moalhaddar/docx-to-pdf, the service this one re-implements and the reference for what converts and how the PDF
# looks, pinned in oracle.env. linux/amd64 only: on an Arm workstation it runs under
# emulation, and a conversion takes seconds instead of a fraction of one. `docx-to-pdf-oracle` is shared by
# every session; a run that needs its own state passes ORACLE_NAME=docx-to-pdf-oracle-<name> and a free
# ORACLE_PORT, and ends with `make oracle-stop` under the same name. Port 48080 keeps clear of the other ports'
# oracles (18080, 1080, 28080, 29000, 38080).
include oracle.env
ORACLE_NAME ?= docx-to-pdf-oracle
ORACLE_PORT ?= 48080

oracle: ## start the oracle on ORACLE_PORT (reuse it if it runs)
	@docker ps -q --filter name=^$(ORACLE_NAME)$$ | grep -q . || \
		docker run -d --name $(ORACLE_NAME) --platform linux/amd64 -p $(ORACLE_PORT):8080 $(ORACLE_IMAGE) > /dev/null
	@$(MAKE) -s oracle-wait

# The oracle can exit during its start (under emulation its LibreOffice connection can miss upstream's fixed 10 s
# limit), and nothing would ever answer: stop waiting as soon as the container no longer runs, show why, and remove it
# so the next `make oracle` starts a fresh one instead of failing on the name.
oracle-wait: ## wait until the oracle's readiness group answers UP; fail if its container stops first
	@until curl -sf http://localhost:$(ORACLE_PORT)/actuator/health/readiness > /dev/null; do \
		state=$$(docker inspect -f '{{.State.Status}}, exit code {{.State.ExitCode}}' $(ORACLE_NAME) 2> /dev/null); \
		case "$$state" in running,*) sleep 2; continue ;; esac; \
		if [ -z "$$state" ]; then echo "oracle: no container $(ORACLE_NAME)" >&2; exit 1; fi; \
		echo "oracle: $(ORACLE_NAME) stopped before it was ready ($$state); its last log lines:" >&2; \
		docker logs --tail 30 $(ORACLE_NAME) >&2; \
		docker rm -f -v $(ORACLE_NAME) > /dev/null; \
		echo "oracle: removed $(ORACLE_NAME); \`make oracle\` starts a fresh one" >&2; exit 1; \
	done; \
		echo "oracle ready on http://localhost:$(ORACLE_PORT)"

oracle-stop: ## remove the oracle
	docker rm -f -v $(ORACLE_NAME)

record: oracle ## record the scenario goldens from the oracle (tests/scenarios/README.md); ONLY=a,b limits it
	DOCX_TO_PDF_RECORD_ORACLE=http://localhost:$(ORACLE_PORT) $(if $(ONLY),DOCX_TO_PDF_RECORD_ONLY=$(ONLY)) \
		cargo test --test oracle scenarios -- --exact --nocapture

# The service's image (Dockerfile). `make image` builds the host's architecture only and loads it;
# `make image-multiarch` builds linux/amd64 and linux/arm64 together (PUSH=<registry/repo:tag> pushes the
# manifest; without it the result stays in the build cache). On Apple Silicon the amd64 half runs apt and
# fc-cache under emulation: build it when the machine is not busy.
IMAGE ?= docx-to-pdf:dev
IMAGE_ARCH ?= $(subst aarch64,arm64,$(subst x86_64,amd64,$(shell uname -m)))
# Every build labels the commit it was built from (org.opencontainers.image.revision), so a measurement or a bug
# report names the image's source, not only the checkout it was run from. A build from a checkout whose tracked files
# differ from HEAD gets `<commit>-dirty` (`git describe --dirty`'s mark, the bench's own test): the image is not that
# commit, and a bare hash would say it is. Outside a git checkout there is no label; IMAGE_REVISION= sets it by hand.
IMAGE_REVISION ?= $(shell git rev-parse HEAD 2> /dev/null)$(if $(shell git status --porcelain --untracked-files=no 2> /dev/null),-dirty)
IMAGE_LABELS = $(if $(IMAGE_REVISION),--label org.opencontainers.image.revision=$(IMAGE_REVISION))

.PHONY: image image-multiarch image-test image-egress image-scenarios image-corpus corpus-record chart-kind

image: ## build the image for the host's architecture and load it as IMAGE (docx-to-pdf:dev)
	docker buildx build --platform linux/$(IMAGE_ARCH) $(IMAGE_LABELS) --load -t $(IMAGE) .

image-multiarch: ## build the image for linux/amd64 and linux/arm64; PUSH=<ref> pushes the manifest
	docker buildx build --platform linux/amd64,linux/arm64 $(IMAGE_LABELS) $(if $(PUSH),--push -t $(PUSH)) .

image-test: ## image/test.sh against IMAGE: fonts, fc-match, the chart's security context, tmpfs, a mounted font
	image/test.sh $(IMAGE)

image-egress: ## image/egress-test.sh against IMAGE: no outbound connection from a document's external references
	image/egress-test.sh $(IMAGE)

image-scenarios: ## replay every scenario against IMAGE run under the chart's security context
	tests/oracle/image-scenarios.sh $(IMAGE)

image-corpus: ## the corpus against IMAGE: report in target/corpus/report.md
	tests/oracle/image-scenarios.sh $(IMAGE) corpus

chart-kind: ## install charts/docx-to-pdf into a throwaway kind cluster, run its helm test and convert through its Service (IMAGE=docx-to-pdf:dev)
	tools/chart/kind-test.sh $(IMAGE)

corpus-record: oracle ## record the corpus goldens from the oracle (tests/corpus/README.md); ONLY=a,b limits it
	DOCX_TO_PDF_RECORD_ORACLE=http://localhost:$(ORACLE_PORT) $(if $(ONLY),DOCX_TO_PDF_RECORD_ONLY=$(ONLY)) \
		cargo test --test corpus corpus -- --exact --nocapture

# CI: one workflow, .gitriver/workflows/ci.yml on the private mirror's GitRiver and .github/workflows/ci.yml on GitHub
# Actions, the same jobs in both, so a gate cannot drift between them; one pipeline per push, so a push has one
# result. Its image job is `make image-ci`, run where a Docker daemon, cargo and curl are; on GitRiver that is the job
# on the runner whose jobs get a Docker socket (the other runners give none), in parallel with the others. No registry: nothing is
# pushed.
.PHONY: ci-job image-ci image-quickstart image-installation

ci-job: ## run one GitRiver CI job (JOB=lint-<gate>|test|lockhash) in its image on a copy of the working tree; the image job is `make image-ci`
	ruby tools/ci/run-job.rb $(JOB)

image-ci: ## the image job: build IMAGE for the host's architecture, then image-test, image-quickstart, image-installation, image-egress, image-scenarios, image-corpus, chart-kind, image-lint
	$(MAKE) image IMAGE=$(IMAGE)
	$(MAKE) image-test IMAGE=$(IMAGE)
	$(MAKE) image-quickstart IMAGE=$(IMAGE)
	$(MAKE) image-installation IMAGE=$(IMAGE)
	$(MAKE) image-egress IMAGE=$(IMAGE)
	$(MAKE) image-scenarios IMAGE=$(IMAGE)
	$(MAKE) image-corpus IMAGE=$(IMAGE)
	$(MAKE) chart-kind IMAGE=$(IMAGE)
	$(MAKE) image-lint IMAGE=$(IMAGE)

# The first things a reader types, against IMAGE: the README's quick start, its own `docker run` and
# `curl` lines read from README.md; and the installation guide's `#[ignore]`d Docker and helm tests (compose, helm
# template), which need Docker Compose 2.24 or later, helm and curl.
image-quickstart: ## the README's quick start (its docker run and curl lines) against IMAGE: 200 and a PDF
	sh tools/ci/quickstart.sh $(IMAGE)

image-installation: ## DOCX_TO_PDF_IMAGE=IMAGE cargo test --test installation -- --ignored (compose up, helm template)
	sh tools/ci/installation-docker.sh $(IMAGE)

# The image gates (tools/lint/image.sh): BuildKit's checks on the Dockerfiles, then Trivy, Dockle, the
# size budget and the SBOM on IMAGE, each tool pinned in tools/lint/images.Dockerfile. They need the daemon and the
# image, so CI runs them last in `make image-ci`: a CVE published overnight does not hide the behaviour tests' result.
.PHONY: image-lint image-size-record

image-lint: ## the image gates against IMAGE: build-check, trivy, dockle, size, sbom (target/sbom/); ONLY=<gate>[,<gate>]
	sh tools/lint/image.sh $(IMAGE) $(ONLY)

image-size-record: ## write IMAGE's size as its architecture's baseline in tools/lint/image-size.txt (at a release)
	sh tools/lint/image.sh --record-size $(IMAGE)

# The bench (tools/bench/README.md): the oxidestack/docx-to-pdf image under load, what it costs, the graphs.
BENCH = $(PY) tools/bench/bench.py
REPS ?= 3

.PHONY: bench-tools bench-generate bench-matrix bench-report

bench-tools: ## build the bench's tool image (generator, sampler, analysis)
	$(BENCH) tools

bench-generate: ## the bench's documents into .bench/docs/ (HOSTILE=1 adds the hostile ones)
	$(BENCH) generate $(if $(HOSTILE),--hostile)

bench-matrix: ## run MATRIX=tools/bench/matrices/<name>.tsv, REPS replicates (3), resumable; IMAGE=docx-to-pdf:bench
	$(BENCH) matrix $(MATRIX) --reps $(REPS) $(if $(filter-out docx-to-pdf:dev,$(IMAGE)),--image $(IMAGE))

bench-report: ## summaries and graphs of every finished run into .bench/report/
	$(BENCH) report

# The comparison anyone can rerun (examples/compare/README.md): oxidestack/docx-to-pdf, Gotenberg and
# moalhaddar/docx-to-pdf (amd64 hosts only), the speed cell (one pinned vCPU, pool 1, the typical report mix) and the
# torture test (hostile documents, each to a fresh container of each service), tables printed and kept in
# .bench/compare/<stamp>/ with the raw results. Only Docker is needed: the runner is a container.
.PHONY: compare

compare: ## speed, memory and torture tables for the three services; QUICK=1 for minutes on a laptop; IMAGE= tests that image (default: build docx-to-pdf:compare)
	$(if $(filter-out docx-to-pdf:dev,$(IMAGE)),IMAGE=$(IMAGE)) IMAGE_REVISION='$(IMAGE_REVISION)' tools/compare/compare.sh $(if $(QUICK),--quick)
