# Releasing: the notes in CHANGELOG.md and the release workflow

A release's notes are written with the changes, by their authors: each commit's `release-note` block (CONTRIBUTING.md,
"Commit messages") is an entry. At a release they are drafted into `CHANGELOG.md`, read and edited by one person, and
that edited section is what the GitHub release says.

## The steps

1. **Draft.** On an up-to-date `main` with a clean tree, run `make release-notes`. The pinned git-cliff (`cliff.toml`,
   its image in `tools/lint/images.Dockerfile`) reads the commits since the last `v*` tag and writes the next
   release's section under the head of `CHANGELOG.md`; it prints the version the commits call for: a breaking change
   a major, a `feat` a minor, a `fix` a patch, and `1.0.0` before the first tag. Each entry is a block's text with
   the commit's scope and short hash, under its section: Action required (a breaking commit, whatever its type),
   Features, Bug fixes, Performance, Other changes. A block saying `NONE`, and a commit without one, give no entry.
2. **Commit the draft as it came**, with the version bumps the release needs (the chart's `version` and `appVersion`,
   `Cargo.toml`): `chore(release): draft vX.Y.Z`.
3. **Edit the section by hand** and commit it alone: `chore(release): vX.Y.Z`. Merge duplicates, reword what is unclear
   to someone who runs the service, add an introduction when the release needs one, and set the heading's date to
   the day of the tag. The diff between the two commits is the edit; nothing else changes in this one.
4. **Tag the edited commit** `vX.Y.Z` (annotated: `git tag -a vX.Y.Z -m vX.Y.Z`) and push the commit, then the tag.
   The tag starts `.github/workflows/release.yml` (below).
5. **The release body is that section**: the workflow takes the text of `## [X.Y.Z]` in `CHANGELOG.md` at the tag,
   from the line after the heading up to the next `## [`, without the compare link at its end, and appends an
   Install and a Verify block: the `docker run` and `helm install` of the version, and the `cosign verify` and
   `gh attestation verify` commands that check its signatures and attestations, with the cosign the files' Sigstore
   bundles need (`tools/ci/release.sh notes`). It never uses git-cliff's output and never GitHub's generated notes.

6. **The chart's changelog on Artifact Hub is that section too**: `tools/ci/release.sh chart` writes the packaged
   chart's `artifacthub.io/changes` from it, one entry per item, its kind the item's section: Features `added`, Bug
   fixes `fixed`, Action required, Performance and Other changes `changed` (a hand-added Security, Deprecated or
   Removed section gives its own kind). The introduction and the entries' `(scope, hash)` are left out.
   `charts/docx-to-pdf/Chart.yaml` carries no `changes` of its own: the packaging refuses one.

A pre-release tag (`vX.Y.Z-rc.N`) needs none of steps 1-3: no section in `CHANGELOG.md` and no version bump, its
chart is packaged at the tag's version without `artifacthub.io/changes`, its body says it is a pre-release, and
`latest` stays on the last release.

To see what the next release would say without touching anything, run step 1 and `git checkout CHANGELOG.md`.

## The release workflow

One tag publishes every artifact, quay.io first; the header of `.github/workflows/release.yml` describes each job.

- **gate**: the tag is a SemVer tag on `main`; for a release, the chart's `version` and `appVersion`,
  `Cargo.toml`'s version and the `CHANGELOG.md` section say the same version; then it waits until `ci.yml` has
  passed on that commit. Nothing is built for a tag that fails here.
- **image**: the CI's image job on native amd64 and arm64 runners, with the SBOMs.
- **publish** (the `release` environment): the image and the chart pushed to `quay.io/oxidestack`, signed with
  cosign keyless, attested (SLSA provenance, and an SPDX SBOM for each architecture) and verified. A failure here
  fails the release.
- **mirror**: the same digests, signatures and attestations copied to `ghcr.io/oxidestack`. A mirror failure is a
  warning, never a failed release.
- **github-release**: the GitHub release with the body of step 5, the SBOMs, the chart's archive and the compose
  examples. Each file is signed with cosign keyless, its Sigstore bundle attached as `<file>.sigstore.json`, and one
  SLSA build provenance of them all (actions/attest, also kept at GitHub) is attached as
  `docx-to-pdf-<version>.intoto.jsonl`; both are verified before the release is made. These are what OpenSSF
  Scorecard's Signed-Releases check reads: a `.sigstore.json` asset on each of the last five releases, and an
  `.intoto.jsonl` one for the full score.
- **index**: the GitHub release gets `index.yaml`, the Helm repository index of every release's chart, which
  `helm repo add docx-to-pdf https://github.com/oxidestack/docx-to-pdf/releases/latest/download` reads, signed and
  attested the same way (`index.yaml.sigstore.json`, `index.yaml.intoto.jsonl`). The documentation site deploys from
  `main`.

A manual run (`gh workflow run release.yml`) is the dry run: both architectures built and tested, the chart and the
notes made at version `0.0.0-dry-run`, the chart and the index signed and verified with a throwaway key (`release.sh
sign-blob` and `verify-blob` with `COSIGN_KEY`/`COSIGN_PUB`), nothing pushed or published. A user checks a release as
[Verifying a release](../../docs/guide/installation.md#verifying-a-release) says.

## What is tested

`make tools-test` runs `tools/release/test_release_notes.py`: it builds a throwaway history (`fixture.py`) with a
feature, a fix, a breaking change, a `NONE`, commits without a block and one with an optional block, after a tagged
release and before any, runs `make release-notes REPO=<it>` and compares the result with `expected-tagged.txt` and
`expected-untagged.txt`. It needs Docker for the drafts and skips them without it; with `DOCX_TO_PDF_REQUIRE_DOCKER=1`
it fails instead. CI runs them in the `release-notes` job, on the runner that gives its jobs the host's Docker socket,
with `RELEASE_NOTES_VOLUMES_FROM` naming the job's own container (`tools/release/release-notes.sh` says why). A change
to `cliff.toml`'s template or to `CHANGELOG.md`'s head changes those files in the same commit.

`tools/release/test_chart_changes.py`, in the same runs, checks the chart's `artifacthub.io/changes` (step 6):
`release.sh changes` on `expected-tagged.txt` as drafted, on `changes-changelog.txt` (wrapped entries, quotes, a
backslash, hand-written entries, a Security section, the next release's section) and on `CHANGELOG.md`'s first
release, and that a pre-release without a section gets none. It needs only `sh` and `awk`.

`tools/release/test_release_body.py`, in the same runs, checks the release body (step 5): `release.sh notes` on
`expected-tagged.txt` gives the section without its compare link, then the Install and Verify blocks with the version,
the digest and the workflow's identity, and the Verify block says that the files' bundles need cosign 3 or later
(cosign 2 only with `--new-bundle-format`). It needs only `sh` and `awk`.
