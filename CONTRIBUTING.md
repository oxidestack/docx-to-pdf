# Contributing to docx-to-pdf

Thank you for helping. This page says where to ask, how to build and test the service, which checks a change has to
pass, and how to write the commits of a pull request.

## Where to go

- **A question** (how to deploy it, why a document looks the way it does, whether it fits your case): start a
  [discussion in Q&A](https://github.com/oxidestack/docx-to-pdf/discussions/categories/q-a).
- **An idea** you would like to talk through before it becomes a request:
  [Ideas](https://github.com/oxidestack/docx-to-pdf/discussions/categories/ideas).
- **A bug**: [open an issue](https://github.com/oxidestack/docx-to-pdf/issues/new/choose) with the document, the
  request and the answer. A document that converts badly is the most useful bug report there is; if it is
  confidential, a smaller document that shows the same thing works as well.
- **A vulnerability**: never in a public issue. [`SECURITY.md`](SECURITY.md) says how to report it privately.

Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).

## Building

The repository carries moalhaddar/docx-to-pdf as a git submodule, which the tests read, so clone it with its
submodules:

```sh
git clone --recursive https://github.com/oxidestack/docx-to-pdf.git
```

In a clone made without `--recursive`, run `git submodule update --init` once.

- `cargo build` builds the service; the Rust version it needs is `rust-version` in `Cargo.toml`.
- `make image` builds the container image for your machine's architecture as `docx-to-pdf:dev`; it needs Docker.

## Testing

- `make check` runs formatting, clippy with warnings as errors, and the library tests. Run it before every commit.
- `cargo test` runs every test. It needs poppler on `PATH` (`pdfinfo`, `pdftotext`, `pdftoppm`: `brew install poppler`
  on macOS, `apt-get install poppler-utils` on Debian), and the tests that convert real documents need LibreOffice.
  `tools/instance-test/run.sh` runs them in a Linux container that carries both.
- `make image-test` and `make image-scenarios` check the image you built: its fonts and security context, and every
  recorded request replayed against it. `make chart-kind` installs the Helm chart into a throwaway kind cluster.

PDFs are never compared byte for byte: LibreOffice writes a different file every time. The tests compare two PDFs by
their pages, text and rendering, so a change that moves a line or drops a picture fails and a new timestamp does not.

## The checks

`make lint` runs every quality gate the CI runs, each in its own container pinned by digest, so it needs Docker and
nothing else. `make lint ONLY=<gate>[,<gate>]` runs some of them, and `sh tools/lint/lint.sh --list` lists them all:

- Rust: clippy, rustdoc, `cargo deny` (licences and advisories), unused dependencies, typos, the minimum Rust version,
  the coverage floor.
- The rest of the repository: shell scripts, Python, YAML, Markdown, links, `.editorconfig`, leaked credentials, the
  CI workflows, the Helm chart.
- The image: `make image-lint` (Trivy, Dockle, the size budget, the SBOM).

A pull request runs the same gates; a red one tells you which gate and why.

## Commit messages

Every commit follows [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/) and is signed off
([DCO](https://developercertificate.org/)); a check on every pull request holds both. The release notes are drafted
from the commits' release-note blocks.

````text
<type>(<scope>)!: <description>

<body>

```release-note
<what changes for users and operators, and what they have to do>
```

BREAKING-CHANGE: <what changes and what to do>
Signed-off-by: Your Name <you@example.org>
````

- **Type**: `feat` (something new a client or operator can use), `fix` (what ships behaved wrongly), `perf`,
  `refactor`, `test`, `docs`, `build`, `ci`, `chore`, `revert`.
- **Scope**, optional, the part changed: `api`, `convert`, `pool`, `instance`, `pictures`, `config`, `telemetry`,
  `probe`, `pdf-equivalence`, `image`, `chart`, `examples`, `bench`, `fuzz`, `upstream`, `deps`, `release`. Leave it
  out when the change spans several.
- **Description**: imperative, lower-case, no full stop; the whole first line at most 72 characters.
- **Body**: what changed and why, wrapped at 72 columns. A line that cannot be wrapped is exempt: one without a space
  (a URL, a path), one indented by four spaces or more, one in a fenced block other than the release note (a
  command), and a `Signed-off-by:` or `Co-authored-by:` trailer, whose name and address no trailer can fold.
- **Release note**: a `release-note` block, the body's last paragraph, written for the people who run the service,
  not for its developers. Required on `feat`, `fix`, `perf` and breaking commits, optional on the others. Write
  `NONE` in it when no released version is affected (a bug introduced since the last release); a breaking change
  always has a note. No line in it starts with `#`: git drops such lines when the message goes through the editor.
- **Breaking** — a client or a deployment must change to keep working (an HTTP path or answer, a setting, a chart
  value, the image's user or paths, a metric's name): `!` before the colon, a `BREAKING-CHANGE:` footer, and a
  release note saying what to do.
- **Sign-off**: `git commit -s` adds `Signed-off-by:` as the last line. It certifies the
  [Developer Certificate of Origin](https://developercertificate.org/).
- **Reverts**: `revert: <the reverted commit's first line>`, the reason, and `Refs: <hash>`.

Dependency updates come from [Renovate](https://docs.renovatebot.com/) (`.github/renovate.json5`): its pull requests
are `build(deps)` commits without a release note, signed off by its bot, and go through the same checks as yours.

Pull requests are merged with "Rebase and merge", so each commit lands as written: squash your fixups
(`git commit --fixup`, `git rebase -i --autosquash`) before asking for a merge.

### Signing off

The sign-off is your statement, under the [Developer Certificate of Origin](https://developercertificate.org/), that
you wrote the change or otherwise have the right to submit it under the project's licences. There is no contributor
licence agreement. Sign off with the name and address your commits are authored with:

```sh
git config user.name "Your Name"
git config user.email you@example.org
git commit -s
```

The `lint-commits` check on every pull request fails a commit whose last footer is not its author's `Signed-off-by:`,
and so does the pre-push hook (`make hooks`); `make lint ONLY=commits` runs the same check on your branch, and
`python3 tools/lint/commits.py --message <file>` on a message before you commit it.

A commit you forgot to sign off is signed off again with `git commit --amend -s --no-edit`, or, for every commit of a
branch, `git rebase --signoff main`; then push the branch with `--force-with-lease`.

## Pull requests

- One change per pull request, with the tests that show it: a bug fix comes with the test that failed before it.
- A change to what a client sends or gets back, a setting, a chart value or a metric updates the guide that documents
  it (`docs/guide/`, the chart's `README.md.gotmpl`) in the same pull request.
- For a larger change, start with an issue or a discussion, so that the approach is agreed before the code is written.

## Licences

By contributing you agree that your code is licensed under the [Apache License, Version 2.0](LICENSE) and your
documentation under [Creative Commons Attribution 4.0 International](https://creativecommons.org/licenses/by/4.0/),
as the rest of the repository. A document, a font or code taken from elsewhere keeps its own licence: say where it
comes from and under which licence in the pull request, and it is attributed in [`NOTICE`](NOTICE).
