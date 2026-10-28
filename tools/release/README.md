# Releasing: the notes in CHANGELOG.md

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
5. **The release body is that section**: the release workflow takes the text of `## [X.Y.Z]` in `CHANGELOG.md` at the
   tag, from the line after the heading up to the next `## [`, without the compare link at its end. It never uses
   git-cliff's output and never GitHub's generated notes.

To see what the next release would say without touching anything, run step 1 and `git checkout CHANGELOG.md`.

## What is tested

`make tools-test` runs `tools/release/test_release_notes.py`: it builds a throwaway history (`fixture.py`) with a
feature, a fix, a breaking change, a `NONE`, commits without a block and one with an optional block, after a tagged
release and before any, runs `make release-notes REPO=<it>` and compares the result with `expected-tagged.txt` and
`expected-untagged.txt`. It needs Docker for the drafts and skips them without it; with `DOCX_TO_PDF_REQUIRE_DOCKER=1`
it fails instead. CI runs them in the `release-notes` job, on the runner that gives its jobs the host's Docker socket,
with `RELEASE_NOTES_VOLUMES_FROM` naming the job's own container (`tools/release/release-notes.sh` says why). A change
to `cliff.toml`'s template or to `CHANGELOG.md`'s head changes those files in the same commit.
