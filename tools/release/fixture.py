"""A throwaway git history for the tests of `make release-notes`: one commit of every kind the draft treats
differently, after a tagged release whose commits it must leave out.

    python3 tools/release/fixture.py <empty directory> [--untagged]

Every author, committer and date is fixed, so the commit hashes, and the draft's entries that name them, are the same
on every machine. `--untagged` leaves out the tag: the history of a first release.
"""

import os
import subprocess
import sys
import tomllib

# The repository's root: cliff.toml's header is the fixture's CHANGELOG.md, the file the draft is prepended to.
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

SIGN_OFF = "Signed-off-by: Jane Doe <jane@example.org>"

# (message, the tag put on that commit or None), oldest first. The bodies follow the commit format: the release-note
# block is the body's last paragraph, the footers one trailer block after it.
COMMITS = [
    ("chore: start the public history", None),
    (
        "feat(api): accept DOTM templates\n\n"
        "```release-note\nA Word macro-enabled template (DOTM) is converted like a DOCX.\n```",
        "v1.0.0",
    ),
    (
        "feat(chart): add a priorityClassName value\n\n"
        "```release-note\nThe chart takes `priorityClassName` for the pods.\n```",
        None,
    ),
    (
        "fix(pool): replace an instance that dies during start\n\n"
        "A failed start was retried once and then left the slot empty, so the\n"
        "pool ran one instance short until restart.\n\n"
        "```release-note\n"
        "A LibreOffice instance that dies while starting is replaced, so the\n"
        "service no longer runs with one instance fewer than configured.\n"
        "```",
        None,
    ),
    (
        "feat(config)!: drop the pool.size alias\n\n"
        "```release-note\n"
        "`pool.size` is no longer read. A deployment that sets it sets\n"
        "`DOCX_TO_PDF_POOL_SIZE` (or `POOL_SIZE`) to the same value instead.\n"
        "```\n\n"
        "BREAKING-CHANGE: a deployment that sets pool.size must set\n"
        " DOCX_TO_PDF_POOL_SIZE instead; the value is unchanged.",
        None,
    ),
    (
        "fix(api): close the spool file before the conversion reads it\n\n"
        "No release has carried the bug.\n\n"
        "```release-note\nNONE\n```",
        None,
    ),
    ("build(deps): bump tokio from 1.47.1 to 1.48.0", None),
    (
        "perf(pictures): decode JPEGs at the target scale\n\n"
        "```release-note\nA document with large photographs converts faster and in less memory.\n```",
        None,
    ),
    (
        "docs: describe the mounted font directory\n\n"
        "```release-note\nThe operations guide says how to add fonts without rebuilding the image.\n```",
        None,
    ),
    (
        "fix: answer 413 when the upload is larger than the limit\n\n"
        "```release-note\nAn upload over the size limit is answered 413 instead of 400.\n```",
        None,
    ),
    ("test(convert): cover the empty document", None),
]


def git(repo, *args, when=None):
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": repo,
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_AUTHOR_NAME": "Jane Doe",
        "GIT_AUTHOR_EMAIL": "jane@example.org",
        "GIT_COMMITTER_NAME": "Jane Doe",
        "GIT_COMMITTER_EMAIL": "jane@example.org",
    }
    if when:
        env["GIT_AUTHOR_DATE"] = env["GIT_COMMITTER_DATE"] = when
    subprocess.run(["git", "-C", repo, *args], env=env, check=True, capture_output=True)


def build(repo, tagged=True):
    """Builds the history in `repo`, an empty directory; its CHANGELOG.md is the repository's head alone (cliff.toml's
    header), without the releases the repository's file has drafted since."""
    git(repo, "init", "-q", "-b", "main")
    with open(os.path.join(ROOT, "cliff.toml"), "rb") as src:
        changelog = tomllib.load(src)["changelog"]["header"]
    for n, (message, tag) in enumerate(COMMITS):
        when = f"2026-01-{n + 1:02d}T12:00:00+00:00"
        name = "CHANGELOG.md" if n == 0 else f"change-{n:02d}.txt"
        with open(os.path.join(repo, name), "w", encoding="utf-8") as out:
            out.write(changelog if n == 0 else f"{n}\n")
        git(repo, "add", name)
        # The sign-off joins a footer block the message ends with, as `git commit -s` does.
        sep = "\n" if message.rsplit("\n\n", 1)[-1].startswith("BREAKING-CHANGE:") else "\n\n"
        git(repo, "commit", "-q", "--cleanup=verbatim", "-m", f"{message}{sep}{SIGN_OFF}", when=when)
        if tag and tagged:
            git(repo, "tag", tag, when=when)


if __name__ == "__main__":
    if len(sys.argv) not in (2, 3) or (len(sys.argv) == 3 and sys.argv[2] != "--untagged"):
        sys.exit(__doc__)
    build(sys.argv[1], tagged=len(sys.argv) == 2)
