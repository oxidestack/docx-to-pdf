#!/usr/bin/env python3
"""The commit-message gate: every commit of the public history is a Conventional Commit in this project's form.

    python3 tools/lint/commits.py                       the fixtures, then every commit from the base to HEAD
                                                        (`make lint ONLY=commits`)
    python3 tools/lint/commits.py --pre-push <remote>   the commits each ref on stdin pushes that descend from the
                                                        base (`make commits-check PUSH=<remote>`, the pre-push hook)
    python3 tools/lint/commits.py --message <file>      one message, e.g. before `git commit -F <file>`

The rules (CONTRIBUTING.md's "Commit messages"), each a rule id the findings name:

- header-form: `<type>[(<scope>)][!]: <description>`; type: one of TYPES; scope: one of SCOPES, or none;
  header-length: at most 72 characters; description-case: no capitalised first word (a name keeps its case, so a
  word with a capital inside, all capitals, a digit or `_` passes); description-stop: no full stop at the end;
- body-blank-line: a blank line after the header; body-width: body lines at most 72 columns, except a line without a
  space (a URL, a path), a line indented by four spaces or more, a `Signed-off-by:` or `Co-authored-by:` line (a
  name and an address, which no trailer can fold: a bot's sign-off runs to 78), and lines in a fenced block other
  than the release note (a command);
- breaking: `!` before the colon if and only if a `BREAKING-CHANGE:` footer; breaking-spelling: `BREAKING CHANGE:`
  with a space, which git's trailer parser does not take; footer-form: a footer is `Token: value`, never the
  `Token #value` the Conventional Commits spec also allows, which git's trailer parser does not take either (so
  `git commit -s` would put its sign-off in a paragraph of its own);
- revert: `revert: <the reverted commit's header>` and a `Refs:` footer;
- release-note-required: a ```release-note block on feat, fix, perf and every breaking commit; release-note-count: at
  most one; release-note-empty: not empty; release-note-none: never `NONE` on a breaking commit; release-note-hash:
  no line starting with `#` (git's cleanup strips it); release-note-place: closed, the body's last paragraph
  before the footers, and no paragraph before its end opening as a footer does in git-cliff's parser (FOOTER_START),
  which would end the body there and drop the note from the release notes;
- footers-split: the footers are the last paragraph, one block; footer-key: one of FOOTERS; footer-order: in FOOTERS'
  order (so `Signed-off-by:` last); refs: a `Refs:` value is commit hashes or `#<n>`; closes: a `Closes:` value is
  one `#<n>`, an issue of this repository the merge closes (GitHub reads `Closes: #<n>`; another closing keyword,
  `Fixes:` or `Resolves:`, is a footer-key finding that names `Closes:`);
- merge: no merge commit (pull requests are rebased onto main);
- signoff: the last footer is the author's `Signed-off-by:` (the DCO; a `--message` has no author, so any sign-off
  last). On by default, here, in the CI jobs and in the pre-push hook; COMMITS_SIGNOFF=0 turns it off, and the
  fixtures run without it.

Whether a `NONE` is right, whether the description is imperative and whether a security fix names its advisory are
the reviewer's to judge; no rule here guesses at them.

The base: COMMITS_BASE, else BASE below: the commit the rule starts from (the public history's root commit, or a pull
request's base). BASE is empty until the public history starts, and empty (or COMMITS_BASE=none) checks the fixtures
alone: every commit before it keeps its own style. A base with no parent (a root commit) is checked too. A shallow
clone that lacks the base is deepened once from its default remote; a deepening that fails is exit 2 with git's error.
A published commit a later rule finds is named in EXEMPT with that rule, rather than the history rewritten.

Fixtures: tools/lint/commits/pass/*.txt pass; tools/lint/commits/fail/<rule>[.<n>].txt fail with that rule and no
other. A new rule comes with both.

Exit 0: clean. 1: findings, `<commit or file>: <rule>: <what>`, one per line. 2: a usage error, or git cannot resolve
the range. Python stdlib and git only.
"""

import os
import re
import subprocess
import sys
from pathlib import Path

TYPES = ("feat", "fix", "perf", "refactor", "test", "docs", "build", "ci", "chore", "revert")
# The parts of the product. A new part adds its scope here in the commit that adds the part, with CONTRIBUTING.md.
SCOPES = (
    "api",
    "convert",
    "pool",
    "instance",
    "pictures",
    "config",
    "telemetry",
    "probe",
    "pdf-equivalence",
    "image",
    "chart",
    "examples",
    "bench",
    "fuzz",
    "upstream",
    "deps",
    "release",
)
# The types whose commit carries a release note (so does every breaking commit).
NOTE_REQUIRED = ("feat", "fix", "perf")
# The footers, in their order: the references to other commits and issues first, `Closes:` (the issues the commit
# resolves) after `Refs:` (those it only relates to), then the people, a human co-author's line before the sign-offs,
# `Signed-off-by:` last as `git commit -s` appends it.
FOOTERS = ("BREAKING-CHANGE", "Refs", "Closes", "Co-authored-by", "Signed-off-by")
# GitHub's other closing keywords: each is spelt `Closes:` here, so one footer key closes an issue.
CLOSING = ("close", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved")
# The public history's root commit, set in the commit that starts it; COMMITS_BASE overrides it.
BASE = "661021207ce6bbd9ad7d3fa760ca95254d709792"
# Published commits a later rule finds, each with the rule it is exempt from: the history is not rewritten. 15737ae
# puts `Closes #5` before its release note, so git-cliff dropped the note; its entry was written into CHANGELOG.md's
# 1.3.0 section by hand.
EXEMPT = {"15737aefa27a02830659975d11ac6b832dc10815": "release-note-place"}
HEADER_MAX = 72
BODY_MAX = 72

HEADER = re.compile(r"^(?P<type>\w+)(?:\((?P<scope>[^()]*)\))?(?P<bang>!)?: (?P<desc>\S.*)$")
# A trailer line as git reads one (a key without whitespace), and the spec's spellings git does not take, kept apart
# so they are named rather than splitting the block: `BREAKING CHANGE` with a space, and `Token #value`.
TRAILER = re.compile(r"^(?P<key>[A-Za-z0-9][A-Za-z0-9-]*|BREAKING CHANGE)(?P<sep>: | #)(?P<value>.*)$")
# Where git-cliff's parser (git-conventional 0.12.9, git-cliff 2.10.1's) starts the footers: the first line of the
# body that opens a paragraph (the body's first line, or one after a blank line, fenced or not) with a token and a
# separator. The token is `BREAKING CHANGE`, or any run of characters but whitespace, parentheses, `:` and `!`; the
# separator `:` or ` #`. Everything from there on is footers, so git-cliff's `commit.body`, where cliff.toml looks for
# the release note, ends before it.
# https://github.com/crate-ci/git-conventional/blob/v0.12.9/src/parser.rs
FOOTER_START = re.compile(r"^(?P<token>BREAKING CHANGE|[^\s():!]+)(?P<sep>:| #)")
FENCE = re.compile(r"^(`{3,})(.*)$")
# A trailer naming a person or a bot: exempt from the body width, since it cannot be wrapped.
IDENTITY = re.compile(r"^(?:Signed-off-by|Co-authored-by): ")
REF = re.compile(r"^(?:[0-9a-f]{7,40}|#[0-9]+)$")
ISSUE = re.compile(r"^#[0-9]+$")
# A capitalised plain word (`Add`, `Fix`): a name with a capital inside, all capitals, a digit or `_` is not one.
CAPITALISED = re.compile(r"^[A-Z][a-z]+[,:;]?$")

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "tools" / "lint" / "commits"
ZERO = "0" * 40
NO_BASE = "commits: no base yet (COMMITS_BASE, commits.py's BASE), so no commit is checked (skipped)"


def check_message(message, signoff=False, author=None):
    """The findings for one commit message: a list of (rule, text)."""
    lines = message.replace("\r\n", "\n").rstrip("\n").split("\n")
    lines = [line.rstrip() for line in lines]
    findings = []

    header = lines[0]
    m = HEADER.match(header)
    breaking = False
    kind = None
    if m is None:
        findings.append(("header-form", f"`{header}` is not `<type>[(<scope>)][!]: <description>`"))
    else:
        kind = m["type"]
        breaking = m["bang"] is not None
        if kind not in TYPES:
            findings.append(("type", f"`{kind}` is not one of {', '.join(TYPES)}"))
        if m["scope"] is not None and m["scope"] not in SCOPES:
            findings.append(("scope", f"`{m['scope']}` is not one of {', '.join(SCOPES)}"))
        desc = m["desc"]
        first = desc.split()[0]
        if CAPITALISED.match(first):
            findings.append(("description-case", f"`{first}`: a description starts lower-case (a name keeps its case)"))
        if desc.endswith("."):
            findings.append(("description-stop", "the description ends with a full stop; drop it"))
    if len(header) > HEADER_MAX:
        findings.append(("header-length", f"the header is {len(header)} characters, at most {HEADER_MAX}"))

    rest = lines[1:]
    if rest and rest[0] != "":
        findings.append(("body-blank-line", "a blank line separates the header from the body"))

    # The paragraphs after the header, blank lines inside a fenced block kept inside it. Each paragraph is a list of
    # (line number, text); notes are the release-note blocks as (first line number, text lines, closed).
    paragraphs, notes, current, fence, note, fenced = [], [], [], None, None, set()
    for n, line in enumerate(rest, start=2):
        f = FENCE.match(line)
        if fence is None:
            if line == "":
                if current:
                    paragraphs.append(current)
                current = []
                continue
            current.append((n, line))
            if f:
                fence = f[1]
                if f[2].strip() == "release-note":
                    note = (n, [], False)
                    notes.append(note)
            elif (
                len(line) > BODY_MAX
                and " " in line.strip()
                and not line.startswith("    ")
                and not IDENTITY.match(line)
            ):
                findings.append(("body-width", f"line {n} is {len(line)} columns, at most {BODY_MAX}"))
        else:
            current.append((n, line))
            fenced.add(n)
            if f and f[1] == fence and f[2].strip() == "":
                fence = None
                if note is not None:
                    notes[-1] = (note[0], note[1], True)
                    note = None
            elif note is not None:
                note[1].append(line)
                if len(line) > BODY_MAX and " " in line.strip():
                    findings.append(("body-width", f"line {n} is {len(line)} columns, at most {BODY_MAX}"))
    if current:
        paragraphs.append(current)

    # The footers: the last paragraph, when every line of it is a trailer or a trailer's continuation.
    trailers = []
    if paragraphs and fence is None:
        last = paragraphs[-1]
        if TRAILER.match(last[0][1]) and all(TRAILER.match(t) or t[:1] in (" ", "\t") for _, t in last):
            trailers = [(n, TRAILER.match(t)) for n, t in last if TRAILER.match(t)]
            paragraphs = paragraphs[:-1]
    keys = [t["key"] for _, t in trailers]

    # Where git-cliff ends the body: a release-note block that does not end before it is lost from the release notes.
    # The line that finding names is not named again as a footer outside the footers.
    lost, cut = None, footers_start(lines)
    if cut is not None and notes and notes[-1][2]:
        n, opening = cut
        start, text, _ = notes[-1]
        if n <= start + len(text):
            lost = n
        if n < start:
            findings.append(
                (
                    "release-note-place",
                    f"line {n}: `{opening}` opens a paragraph, so git-cliff reads the footers from there and the"
                    " release-note block after it is dropped from the release notes; move an issue reference into"
                    " the footers after the block (`Closes: #<n>`), or reword the paragraph's start",
                )
            )
        elif n <= start + len(text):
            findings.append(
                (
                    "release-note-place",
                    f"line {n}: `{opening}` opens a paragraph of the release note, so git-cliff reads the footers"
                    " from there and the note is cut short in the release notes; reword the paragraph's start",
                )
            )

    for p in paragraphs:
        for n, line in p:
            t = TRAILER.match(line)
            if t and n not in fenced and n != lost and t["key"] in FOOTERS + ("BREAKING CHANGE",):
                findings.append(
                    (
                        "footers-split",
                        f"line {n}: `{t['key']}{t['sep'].rstrip()}` outside the footers, the last paragraph",
                    )
                )
    rank = -1
    for n, t in trailers:
        key = t["key"]
        value = t["value"].strip()
        if key == "BREAKING CHANGE":
            findings.append(("breaking-spelling", f"line {n}: `BREAKING CHANGE:` is spelt `BREAKING-CHANGE:`"))
            key = "BREAKING-CHANGE"
        if t["sep"] == " #":
            findings.append(
                (
                    "footer-form",
                    f"line {n}: `{key} #{value}` is written `{key}: #{value}`, the form git's trailers take",
                )
            )
            value = "#" + value
        if key not in FOOTERS:
            hint = "; GitHub's closing keywords are spelt `Closes:` here" if key.lower() in CLOSING else ""
            findings.append(("footer-key", f"line {n}: `{key}:` is not one of {', '.join(FOOTERS)}{hint}"))
            continue
        if FOOTERS.index(key) < rank:
            findings.append(
                ("footer-order", f"line {n}: `{key}:` comes before `{FOOTERS[rank]}:` ({', '.join(FOOTERS)})")
            )
        rank = max(rank, FOOTERS.index(key))
        if key == "Refs":
            bad = [r for r in re.split(r"[,\s]+", value) if r and not REF.match(r)]
            if bad or not value:
                findings.append(("refs", f"line {n}: `Refs:` names commit hashes or `#<n>`, not `{' '.join(bad)}`"))
        if key == "Closes" and not ISSUE.match(value):
            findings.append(("closes", f"line {n}: `Closes:` names one issue, `#<n>` (a line for each), not `{value}`"))

    has_footer = any(k in ("BREAKING-CHANGE", "BREAKING CHANGE") for k in keys)
    if breaking and not has_footer:
        findings.append(("breaking", "`!` in the header and no `BREAKING-CHANGE:` footer saying what to do"))
    if has_footer and m is not None and not breaking:
        findings.append(("breaking", "a `BREAKING-CHANGE:` footer and no `!` before the header's colon"))
    breaking = breaking or has_footer

    if kind == "revert":
        inner = HEADER.match(m["desc"])
        if inner is None or inner["type"] not in TYPES:
            findings.append(("revert", "the description is the reverted commit's header"))
        if "Refs" not in keys:
            findings.append(("revert", "a `Refs:` footer names the reverted commit"))

    if len(notes) > 1:
        findings.append(("release-note-count", f"{len(notes)} release-note blocks, at most one"))
    if not notes and (kind in NOTE_REQUIRED or breaking):
        why = "a breaking commit" if breaking else f"a `{kind}` commit"
        findings.append(("release-note-required", f"{why} carries a ```release-note block"))
    for start, text, closed in notes:
        body = [t for t in text if t.strip()]
        if not closed:
            findings.append(("release-note-place", f"line {start}: the release-note block is not closed"))
            continue
        if not body:
            findings.append(("release-note-empty", f"line {start}: the release-note block is empty"))
        elif breaking and [t.strip() for t in body] == ["NONE"]:
            findings.append(("release-note-none", f"line {start}: a breaking commit's release note is never NONE"))
        for t in text:
            if t.startswith("#"):
                findings.append(("release-note-hash", f"line {start}: `{t}`: git drops a line starting with `#`"))
    if notes and notes[-1][2] and paragraphs:
        last_body = paragraphs[-1]
        if last_body[0][0] != notes[-1][0] or FENCE.match(last_body[-1][1]) is None:
            findings.append(("release-note-place", "the release-note block is the body's last paragraph"))

    if signoff:
        last = trailers[-1][1] if trailers else None
        if last is None or last["key"] != "Signed-off-by":
            findings.append(("signoff", "the last footer is the author's `Signed-off-by:` (`git commit -s`)"))
        elif author is not None and author not in [
            t["value"].strip() for _, t in trailers if t["key"] == "Signed-off-by"
        ]:
            findings.append(("signoff", f"no `Signed-off-by: {author}`, the author's"))
    return findings


def footers_start(lines):
    """Where git-cliff's parser starts the footers (FOOTER_START): (line number, the opening token and separator), or
    None when the message has none."""
    body = 1
    while body < len(lines) and lines[body].strip() == "":
        body += 1
    prior_empty = True
    for i in range(body, len(lines)):
        m = FOOTER_START.match(lines[i])
        if prior_empty and m:
            return i + 1, m[0]
        prior_empty = lines[i].strip() == ""
    return None


def check_fixtures():
    """Every passing fixture passes, every failing one fails with its rule alone: the gate tests itself first."""
    problems = []
    passing = sorted((FIXTURES / "pass").glob("*.txt"))
    failing = sorted((FIXTURES / "fail").glob("*.txt"))
    for path in passing:
        found = check_message(path.read_text(encoding="utf-8"))
        if found:
            problems.append(f"{path.relative_to(ROOT)}: should pass, found {[r for r, _ in found]}")
    for path in failing:
        rule = path.name.split(".")[0]
        found = {r for r, _ in check_message(path.read_text(encoding="utf-8"))}
        if found != {rule}:
            problems.append(f"{path.relative_to(ROOT)}: should fail with `{rule}` alone, found {sorted(found)}")
    if not passing or not failing:
        problems.append(f"{FIXTURES.relative_to(ROOT)}: no fixtures")
    return problems, len(passing) + len(failing)


def git(*args, check=True, stdin=None):
    r = subprocess.run(["git", *args], capture_output=True, text=True, input=stdin)
    if check and r.returncode != 0:
        raise Unusable(f"commits: git {' '.join(args)}: {r.stderr.strip()}")
    return r


class Unusable(Exception):
    pass


def resolve_base(base):
    """The base's commit id, deepening a shallow clone (CI's) once when it lacks the base."""
    if git("cat-file", "-e", f"{base}^{{commit}}", check=False).returncode != 0:
        if git("rev-parse", "--is-shallow-repository").stdout.strip() == "true":
            print("commits: a shallow clone without the base; fetching the history", file=sys.stderr)
            # A fetch that fails says why (an unreachable remote, a missing credential): the base being missing
            # alone would hide it.
            fetch = git("fetch", "--quiet", "--unshallow", check=False)
            if fetch.returncode != 0:
                raise Unusable(
                    f"commits: the base `{base}` is not in this shallow clone, and `git fetch --unshallow` failed"
                    f" (exit {fetch.returncode}): {fetch.stderr.strip()}"
                )
    r = git("rev-parse", "--verify", "-q", f"{base}^{{commit}}", check=False)
    if r.returncode != 0:
        raise Unusable(f"commits: the base `{base}` (COMMITS_BASE) is no commit of this clone")
    return r.stdout.strip()


def check_commits(shas, signoff):
    """Check each commit of `shas` (oldest first): one `git log` reads them all, NUL-separated."""
    findings = []
    out = git(
        "log", "--no-walk=unsorted", "--stdin", "-z", "--format=%H%n%P%n%an <%ae>%n%B", stdin="\n".join(shas)
    ).stdout
    for record in filter(None, out.split("\0")):
        sha, parents, author, message = record.split("\n", 3)
        where = sha[:12]
        if len(parents.split()) > 1:
            findings.append(f"{where}: merge: a merge commit; rebase onto the base instead")
            continue
        findings += [
            f"{where}: {rule}: {text}"
            for rule, text in check_message(message, signoff, author)
            if EXEMPT.get(sha) != rule
        ]
    return findings, len(shas)


def descendants(base, revs):
    """The commits of `revs` that descend from base (base itself, if it is a root, included)."""
    pushed = git("rev-list", *revs).stdout.split()
    after = set(git("rev-list", "--ancestry-path", revs[0], f"^{base}").stdout.split())
    if len(git("rev-list", "--parents", "-n", "1", base).stdout.split()) == 1:
        after.add(base)
    return [sha for sha in pushed if sha in after]


def main(argv):
    signoff = os.environ.get("COMMITS_SIGNOFF", "1") != "0"
    base = (os.environ.get("COMMITS_BASE") or BASE).strip()
    if base == "none":
        base = ""
    if argv[:1] == ["--message"] and len(argv) == 2:
        findings = [f"{argv[1]}: {r}: {t}" for r, t in check_message(Path(argv[1]).read_text(), signoff)]
        for f in findings:
            print(f)
        return 1 if findings else 0
    if argv and not (argv[0] == "--pre-push" and len(argv) == 2):
        print(__doc__, file=sys.stderr)
        return 2

    if argv:
        if not base:
            print(NO_BASE, file=sys.stderr)
            return 0
        os.chdir(git("rev-parse", "--show-toplevel").stdout.strip())
        base = resolve_base(base)
        findings, count = [], 0
        for line in sys.stdin.read().splitlines():
            parts = line.split()
            if len(parts) != 4 or parts[1] == ZERO:
                continue
            local, remote = parts[1], parts[3]
            if remote != ZERO and git("cat-file", "-e", f"{remote}^{{commit}}", check=False).returncode == 0:
                revs = [local, f"^{remote}"]
            else:
                revs = [local, "--not", f"--remotes={argv[1]}"]
            shas = descendants(base, revs)
            if shas:
                found, n = check_commits(shas[::-1], signoff)
                findings += found
                count += n
        return report(findings, f"{count} pushed commits")

    problems, n = check_fixtures()
    for p in problems:
        print(p)
    if problems:
        print(f"commits: the gate's own fixtures disagree with it ({len(problems)})", file=sys.stderr)
        return 1
    print(f"commits: {n} fixtures agree with the rules", file=sys.stderr)
    if not base:
        print(NO_BASE, file=sys.stderr)
        return 0
    os.chdir(git("rev-parse", "--show-toplevel").stdout.strip())
    base = resolve_base(base)
    shas = descendants(base, ["HEAD"])
    if not shas:
        return report([], f"no commit after {base[:12]}")
    findings, count = check_commits(shas[::-1], signoff)
    return report(findings, f"{count} commits after {base[:12]}")


def report(findings, what):
    for f in findings:
        print(f)
    if findings:
        print(f"commits: {len(findings)} findings in {what}", file=sys.stderr)
        return 1
    print(f"commits: {what}, all conforming", file=sys.stderr)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except Unusable as e:
        print(e, file=sys.stderr)
        sys.exit(2)
