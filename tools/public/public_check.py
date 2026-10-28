#!/usr/bin/env python3
"""The publication gate: nothing private reaches a pushed tree or commit message — no company's name, nothing of the
author's machine or environment, no internal host, no local path, and (the worklog rule) nothing of the worklog.

    python3 tools/public/public_check.py                     the working copy of every tracked file, the messages of
                                                             the commits no remote has, and HEAD against the archive
                                                             (`make public-check`)
    python3 tools/public/public_check.py --pre-push <remote> the same for each ref git's pre-push hook names on stdin
                                                             (`<local ref> <local sha> <remote ref> <remote sha>`):
                                                             the pushed commit's tree, the pushed commits' messages

Two sets of patterns, each a ripgrep regular expression matched case-insensitively:

- the denylist, which names the private things and so is never tracked: `$PUBLIC_DENYLIST`, default
  `~/.config/oxidestack/public-denylist` (one per organisation: the organisation's other projects share the same
  private names); one pattern per line, `#` starts a comment, blank lines ignored. A missing or empty denylist fails
  the check; `PUBLIC_DENYLIST=none` runs the built-in half alone (a stranger's clone, a fork's CI);
- the built-in local paths and internal host names (BUILT_IN), generic, naming nothing. A placeholder passes:
  `/Users/…`, `/home/<user>/…`, `~/…`, `~/Projects/…` — what follows the directory is not a path component.

The worklog rule: the worklog — the tickets, the decision records, the research notes, the ticket tree and the agents'
files — lives in a private repository of its own, and the code repository never names it. With the rule on, a tracked
worklog path (WORKLOG_DIRS, WORKLOG_FILES, an agents' instruction file anywhere) fails, and so does a reference to the
worklog (REFERENCES) in a tracked file or a checked commit message: a ticket, decision record or research number
(`ticket NNNN`, `Ticket NNNN:`, `tickets NNNN and NNNN`, `ADR NNNN`, `research NNNN`), a ticket's possessive
(`NNNN's`), the worklog's commit conventions (`Land NNNN: …`, `Close NNNN-NNNN`), or a worklog path named in text;
NNNN is four digits, the first a zero. The patterns are case-insensitive and run over a whole text, so a reference
split across lines, comment leaders between its words included, is found. A commit message is held stricter still
(MESSAGE_REFERENCES): any number shaped like a ticket's, `NNNN` or `NNNNb`, standing alone fails.
`PUBLIC_WORKLOG=on|off` sets the rule; unset, WORKLOG_DEFAULT does: `on`, since the worklog moved to a repository of
its own (`off` checks a tree from before the move).

A ref that contains the archive (the tag `pre-publication`, the private history before the public repository's one
fresh root commit) is refused. The tag is looked up locally,
then on the remote `$PUBLIC_ARCHIVE_REMOTE` (default `private`, the private mirror's remote) when that
remote exists.

Every pattern is matched against each file's path as well as its contents: a name in a directory or a file name is
published too.

Exit 0: clean. 1: findings, one line each (`path:line: pattern`, `path: path: pattern`, `commit <sha>: line N:
pattern`, `path: worklog path`, `path:line: worklog reference: <text> (<kind>)`, `refused: …`).
2: no usable denylist, a bad pattern, a bad PUBLIC_WORKLOG, or a missing tool (git, rg). Python stdlib, git and
ripgrep only.
"""

import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

DEFAULT_DENYLIST = "~/.config/oxidestack/public-denylist"
ARCHIVE_TAG = "pre-publication"
ZERO = "0" * 40
# A path component: a real path names a user or a project here; `…`, `<user>`, `*` or a backtick do not.
COMPONENT = r"[a-z0-9._-]"
BUILT_IN = [
    ("local path", rf"/Users/{COMPONENT}"),
    ("local path", rf"/home/{COMPONENT}"),
    ("local path", rf"~/Projects/{COMPONENT}"),
    # The sibling project's directory, spelt in two pieces so this file does not match its own pattern.
    ("local path", "~/docx" + r"2pdf\b"),
    ("internal host", r"\b[a-z0-9][a-z0-9-]*(\.[a-z0-9-]+)*\.(localhost|local|internal)\b"),
]
# A private-range address is not built in: the tree's are test and example data (a Kubernetes service address, the
# egress test's private network), which no generic pattern tells from the stand's. Those go in the denylist.
# Names every Docker or Kubernetes install answers to; they name no infrastructure of ours.
GENERIC_HOST_SUFFIXES = (".docker.internal", ".cluster.local")

# The worklog rule's state when PUBLIC_WORKLOG is unset: `on`, since the worklog lives in a repository of its own.
# `PUBLIC_WORKLOG=off` checks a tree from before the move.
WORKLOG_DEFAULT = "on"
# The worklog's paths: its directories, then its files. An agents' instruction file is the worklog's wherever it sits.
# A file's name is spelt in two pieces so that this file does not name a worklog path itself.
WORKLOG_DIRS = ("tickets", "docs/adr", "docs/research", "docs/agents", ".claude", "prototypes")
WORKLOG_FILES = tuple(
    name + ext
    for name, ext in (
        ("docs/tree", ".md"),
        ("docs/milestones", ".md"),
        ("docs/divergences", ".md"),
        ("docs/positioning", ".md"),
        ("CONTEXT", ".md"),
        ("tools/ticket_tree", ".py"),
        ("tools/tests/test_ticket_tree", ".py"),
    )
)
AGENT_FILE = "CLAUDE" + ".md"
# Between the words of a reference: blanks and line breaks with any comment leader (`//`, `#`, `*`, `--`, `;`, `>`,
# `!`) on the next line, or a hyphen.
SEP = rb"[\s#/*;>!-]+"
# A worklog number: four digits, the first a zero (the tree's numbering), and a letter for a split ticket.
NUMBER = rb"0\d{3}[a-z]?\b"
REFERENCES = [
    (
        "a ticket, decision record or research number",
        rb"\b(?:tickets?|adrs?|research(?:" + SEP + rb"notes?)?|items?|branch(?:es)?)" + SEP + NUMBER,
    ),
    ("a decision record or research number", rb"\b(?:adrs?|research)" + SEP + rb"\d{4}\b"),
    ("a ticket's possessive", rb"\b0\d{3}[a-z]?(?:'|\xe2\x80\x99)s\b"),
    ("a worklog commit convention", rb"\b(?:land(?:s|ed|ing)?|close[sd]?|closing|file[sd]?|filing)" + SEP + NUMBER),
    ("a range of tickets", rb"\b0\d{3}[a-z]?[ \t]*(?:-|\xe2\x80\x93)[ \t]*" + NUMBER),
    (
        "a worklog path",
        rb"(?<![\w.-])(?:"
        + b"|".join(
            re.escape(name.encode())
            for name in [d + "/" for d in WORKLOG_DIRS] + list(WORKLOG_FILES) + [AGENT_FILE, "ticket_tree" + ".py"]
        )
        + rb")",
    ),
]
# A code commit's message says what changed and why; no number in it is a ticket's, so one standing alone fails.
MESSAGE_REFERENCES = REFERENCES + [("a ticket number", rb"(?<![\w.:/+-])" + NUMBER + rb"(?![.:/-]?\w)")]


class Unusable(Exception):
    """The check cannot run: exit 2."""


def git(*args, check=True):
    out = subprocess.run(["git", *args], capture_output=True, text=True, env=dict(os.environ, GIT_TERMINAL_PROMPT="0"))
    if check and out.returncode != 0:
        raise Unusable(f"git {' '.join(args)}: {out.stderr.strip()}")
    return out


def denylist_patterns():
    value = os.environ.get("PUBLIC_DENYLIST", "")
    if value == "none":
        return []
    path = Path(os.path.expanduser(value or DEFAULT_DENYLIST))
    if not path.is_file():
        raise Unusable(
            f"public-check: no denylist at {path}; it names the private things, so it is never in the repository. "
            "Create it, point PUBLIC_DENYLIST at one, or run only the built-in patterns with PUBLIC_DENYLIST=none."
        )
    patterns = []
    for number, line in enumerate(path.read_text().splitlines(), 1):
        line = line.strip()
        if line and not line.startswith("#"):
            patterns.append((f"{path}:{number}", line))
    if not patterns:
        raise Unusable(
            f"public-check: the denylist {path} has no pattern (PUBLIC_DENYLIST=none runs the built-in ones)"
        )
    return patterns


def worklog_rule():
    value = os.environ.get("PUBLIC_WORKLOG", "") or WORKLOG_DEFAULT
    if value not in ("on", "off"):
        raise Unusable(f"public-check: PUBLIC_WORKLOG is `{value}`; it is `on` or `off`")
    return value == "on"


def is_worklog_path(path):
    return (
        path in WORKLOG_FILES
        or path.split("/")[-1] == AGENT_FILE
        or any(path.startswith(d + "/") for d in WORKLOG_DIRS)
    )


def find_references(data, references):
    """Each worklog reference in `data` (bytes): yields (line number, kind, the reference with its blanks folded), a
    reference two patterns find reported once."""
    seen = set()
    for kind, pattern in references:
        for match in re.finditer(pattern, data, re.IGNORECASE):
            if match.start() in seen:
                continue
            seen.add(match.start())
            text = b" ".join(match.group(0).split()).decode(errors="replace")
            yield data.count(b"\n", 0, match.start()) + 1, kind, text


def check_worklog(files, cwd):
    """The worklog rule over `files` (relative to cwd): their paths, then their contents."""
    findings = [f"{f}: worklog path" for f in files if is_worklog_path(f)]
    for f in files:
        data = (Path(cwd) / f).read_bytes()
        findings += [f"{f}:{n}: worklog reference: {t} ({k})" for n, k, t in find_references(data, REFERENCES)]
    return findings


def search(patterns, files, cwd):
    """Each (origin, pattern) over `files` (relative to cwd): yields (file, line number, pattern)."""
    files = list(files)
    if not files:
        return
    for origin, pattern in patterns:
        for start in range(0, len(files), 500):
            chunk = files[start : start + 500]
            out = subprocess.run(
                ["rg", "--no-config", "--json", "-i", "-a", "-e", pattern, "--", *chunk],
                cwd=cwd,
                capture_output=True,
                text=True,
                errors="replace",
            )
            if out.returncode not in (0, 1):
                raise Unusable(f"public-check: pattern {origin} `{pattern}`: {out.stderr.strip()}")
            for line in out.stdout.splitlines():
                event = json.loads(line)
                if event["type"] != "match":
                    continue
                data = event["data"]
                texts = [m["match"].get("text", "") for m in data["submatches"]]
                if origin == "internal host" and all(("." + t.lower()).endswith(GENERIC_HOST_SUFFIXES) for t in texts):
                    continue
                yield data["path"].get("text", "?"), data["line_number"], pattern


def search_paths(patterns, paths):
    """Each (origin, pattern) over the file paths themselves: a name in a directory or a file name is published as
    surely as one in a file. Yields (path, pattern)."""
    paths = list(paths)
    if not paths:
        return
    with tempfile.TemporaryDirectory() as tmp:
        listing = Path(tmp) / "paths"
        listing.write_text("".join(p.replace("\n", " ") + "\n" for p in paths))
        for _file, number, pattern in search(patterns, [listing.name], tmp):
            yield paths[number - 1], pattern


def tracked_files(root):
    """Tracked files that are on disk, submodules (mode 160000) left out: they are other repositories."""
    out = git("-C", str(root), "ls-files", "-s", "-z").stdout
    files = []
    for entry in filter(None, out.split("\0")):
        meta, path = entry.split("\t", 1)
        if not meta.startswith("160000") and (root / path).is_file():
            files.append(path)
    return files


def check_working_copy(patterns, root, worklog):
    files = tracked_files(root)
    return (
        [f"{f}: path: {p}" for f, p in search_paths(patterns, files)]
        + [f"{f}:{n}: {p}" for f, n, p in search(patterns, files, root)]
        + (check_worklog(files, root) if worklog else [])
    )


def check_tree(patterns, sha, worklog):
    """The tree of a commit, as `git archive` gives it (submodules are not in it)."""
    with tempfile.TemporaryDirectory() as tmp:
        archive = subprocess.run(["git", "archive", "--format=tar", sha], capture_output=True)
        if archive.returncode != 0:
            raise Unusable(f"git archive {sha}: {archive.stderr.decode(errors='replace').strip()}")
        tar_path = Path(tmp) / "tree.tar"
        tar_path.write_bytes(archive.stdout)
        out_dir = Path(tmp) / "tree"
        out_dir.mkdir()
        with tarfile.open(tar_path) as tar:
            members = [m for m in tar.getmembers() if m.isfile()]
            tar.extractall(out_dir, members=members, filter="data")
        names = [m.name for m in members]
        return (
            [f"{f}: path: {p}" for f, p in search_paths(patterns, names)]
            + [f"{f}:{n}: {p}" for f, n, p in search(patterns, names, out_dir)]
            + (check_worklog(names, out_dir) if worklog else [])
        )


def check_messages(patterns, rev_args, worklog):
    shas = git("rev-list", *rev_args).stdout.split()
    if not shas:
        return []
    with tempfile.TemporaryDirectory() as tmp:
        references = []
        for sha in shas:
            message = git("log", "-1", "--format=%B", sha).stdout
            (Path(tmp) / sha).write_text(message)
            if worklog:
                references += [
                    f"commit {sha[:12]}: line {n}: worklog reference: {t} ({k})"
                    for n, k, t in find_references(message.encode(), MESSAGE_REFERENCES)
                ]
        return [f"commit {f[:12]}: line {n}: {p}" for f, n, p in search(patterns, shas, tmp)] + references


def archive_commit():
    local = git("rev-parse", "-q", "--verify", f"refs/tags/{ARCHIVE_TAG}^{{commit}}", check=False)
    if local.returncode == 0:
        return local.stdout.strip()
    remote = os.environ.get("PUBLIC_ARCHIVE_REMOTE", "private")
    if remote not in git("remote").stdout.split():
        return None
    out = git("ls-remote", "--tags", remote, f"refs/tags/{ARCHIVE_TAG}", f"refs/tags/{ARCHIVE_TAG}^{{}}", check=False)
    if out.returncode != 0:
        print(f"public-check: warning: cannot ask {remote} for the archive: {out.stderr.strip()}", file=sys.stderr)
        return None
    refs = dict(reversed(line.split("\t")) for line in out.stdout.splitlines() if "\t" in line)
    return refs.get(f"refs/tags/{ARCHIVE_TAG}^{{}}") or refs.get(f"refs/tags/{ARCHIVE_TAG}")


def check_archive(archive, sha, name):
    """A ref that contains the archive brings the old history with it. An archive commit we do not have locally
    cannot be in a local commit's history."""
    if archive is None or git("cat-file", "-e", f"{archive}^{{commit}}", check=False).returncode != 0:
        return []
    if git("merge-base", "--is-ancestor", archive, sha, check=False).returncode == 0:
        return [f"refused: {name} contains the archive {ARCHIVE_TAG} ({archive[:12]}); push only the fresh history"]
    return []


def main(argv):
    if len(argv) not in (0, 2) or (argv and argv[0] != "--pre-push"):
        print(__doc__, file=sys.stderr)
        return 2
    for tool in ("git", "rg"):
        if shutil.which(tool) is None:
            raise Unusable(f"public-check: needs `{tool}` on PATH")
    patterns = [(name, p) for name, p in BUILT_IN] + denylist_patterns()
    worklog = worklog_rule()
    root = Path(git("rev-parse", "--show-toplevel").stdout.strip())
    os.chdir(root)
    archive = archive_commit()
    findings = []
    if not argv:
        findings += check_working_copy(patterns, root, worklog)
        if git("rev-parse", "-q", "--verify", "HEAD", check=False).returncode == 0:
            findings += check_messages(patterns, ["HEAD", "--not", "--remotes"], worklog)
            findings += check_archive(archive, "HEAD", "HEAD")
    else:
        remote = argv[1]
        for line in sys.stdin.read().splitlines():
            parts = line.split()
            if len(parts) != 4:
                continue
            local_ref, local_sha, _remote_ref, remote_sha = parts
            if local_sha == ZERO:
                continue
            findings += check_tree(patterns, local_sha, worklog)
            if remote_sha != ZERO and git("cat-file", "-e", f"{remote_sha}^{{commit}}", check=False).returncode == 0:
                findings += check_messages(patterns, [local_sha, f"^{remote_sha}"], worklog)
            else:
                findings += check_messages(patterns, [local_sha, "--not", f"--remotes={remote}"], worklog)
            findings += check_archive(archive, local_sha, local_ref)
    for finding in findings:
        print(finding)
    if findings:
        print(
            f"public-check: {len(findings)} findings: a private name, a local path, an internal host or the worklog",
            file=sys.stderr,
        )
        return 1
    rule = "on" if worklog else "off; PUBLIC_WORKLOG=on runs it"
    print(f"public-check: clean ({len(patterns)} patterns; the worklog rule {rule})", file=sys.stderr)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except Unusable as e:
        print(e, file=sys.stderr)
        sys.exit(2)
