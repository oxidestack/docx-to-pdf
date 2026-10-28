"""`make release-notes` on a fixture history (fixture.py): the drafted section is expected-tagged.txt after a release,
expected-untagged.txt before the first, each under CHANGELOG.md's head. The draft's date is the day it runs, so it is
compared as YYYY-MM-DD.

The drafts need Docker (git-cliff runs in its pinned container) and are skipped without it, unless
DOCX_TO_PDF_REQUIRE_DOCKER=1 says Docker is meant to be there (CI): then they fail instead, so a job that lost its
socket cannot pass by skipping them. The rest needs only Python. The fixture lives under target/, inside the working
tree, because the container mounts it and on macOS the Docker VM sees the home directory, not the system's temporary
one.
"""

import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)

import fixture  # noqa: E402


def header():
    with open(os.path.join(ROOT, "cliff.toml"), "rb") as f:
        return tomllib.load(f)["changelog"]["header"]


def docker():
    if shutil.which("docker") is None:
        return False
    return subprocess.run(["docker", "info"], capture_output=True).returncode == 0


class Head(unittest.TestCase):
    def test_changelog_starts_with_the_configured_head(self):
        # `--prepend` puts the draft under the head only when the file starts with exactly cliff.toml's header.
        with open(os.path.join(ROOT, "CHANGELOG.md"), encoding="utf-8") as f:
            self.assertTrue(f.read().startswith(header()), "CHANGELOG.md does not start with cliff.toml's header")

    def test_git_cliff_is_pinned_by_digest(self):
        with open(os.path.join(ROOT, "tools", "lint", "images.Dockerfile"), encoding="utf-8") as f:
            pins = re.findall(r"^FROM (\S+) AS git-cliff$", f.read(), flags=re.M)
        self.assertEqual(len(pins), 1)
        self.assertRegex(pins[0], r"^orhunp/git-cliff:[0-9.]+@sha256:[0-9a-f]{64}$")


REQUIRE_DOCKER = os.environ.get("DOCX_TO_PDF_REQUIRE_DOCKER") == "1"


@unittest.skipUnless(REQUIRE_DOCKER or docker(), "needs Docker: git-cliff runs in its pinned container")
class Draft(unittest.TestCase):
    def setUp(self):
        if not docker():
            self.fail("DOCX_TO_PDF_REQUIRE_DOCKER=1, but there is no Docker: git-cliff runs in its pinned container")
        base = os.path.join(ROOT, "target", "release-notes")
        os.makedirs(base, exist_ok=True)
        self.repo = tempfile.mkdtemp(dir=base)
        self.addCleanup(shutil.rmtree, self.repo)

    def draft(self, tagged):
        fixture.build(self.repo, tagged=tagged)
        done = subprocess.run(
            ["make", "--no-print-directory", "-C", ROOT, "release-notes", f"REPO={self.repo}"],
            capture_output=True,
            text=True,
        )
        if done.returncode != 0:
            raise AssertionError(f"make release-notes: exit {done.returncode}: {done.stdout}{done.stderr}")
        with open(os.path.join(self.repo, "CHANGELOG.md"), encoding="utf-8") as f:
            text = f.read()
        return done.stdout, re.sub(r"^(## \[[^\]]+\] - )\d{4}-\d{2}-\d{2}$", r"\1YYYY-MM-DD", text, flags=re.M)

    def expected(self, name):
        with open(os.path.join(HERE, name), encoding="utf-8") as f:
            return header() + "\n" + f.read()

    def test_after_a_release(self):
        out, text = self.draft(tagged=True)
        self.assertIn("drafted v2.0.0", out)
        self.assertEqual(text, self.expected("expected-tagged.txt"))

    def test_before_the_first_release(self):
        out, text = self.draft(tagged=False)
        self.assertIn("drafted v1.0.0", out)
        self.assertEqual(text, self.expected("expected-untagged.txt"))


if __name__ == "__main__":
    unittest.main()
