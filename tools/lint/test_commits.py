"""Tests of the commit-message gate (tools/lint/commits.py): the fixtures, the sign-off rule, and the range and
pre-push modes on a throwaway repository. `make tools-test`."""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import commits  # noqa: E402

SCRIPT = HERE / "commits.py"
SIGNED = "Signed-off-by: Jane Doe <jane@example.org>"
GOOD = f"docs: describe the pool settings\n\n{SIGNED}\n"
BAD = f"Describe the pool settings\n\n{SIGNED}\n"


class Fixtures(unittest.TestCase):
    def test_every_fixture_agrees_with_the_rules(self):
        problems, n = commits.check_fixtures()
        self.assertEqual(problems, [])
        self.assertGreater(n, 0)

    def test_every_rule_has_a_failing_fixture(self):
        rules = {p.name.split(".")[0] for p in (commits.FIXTURES / "fail").glob("*.txt")}
        # merge and signoff need a commit or a switch, not a message: the tests below hold them.
        documented = {
            "header-form",
            "type",
            "scope",
            "header-length",
            "description-case",
            "description-stop",
            "body-blank-line",
            "body-width",
            "breaking",
            "breaking-spelling",
            "revert",
            "release-note-required",
            "release-note-count",
            "release-note-empty",
            "release-note-none",
            "release-note-hash",
            "release-note-place",
            "footers-split",
            "footer-key",
            "footer-order",
            "refs",
        }
        self.assertEqual(rules, documented)
        for rule in documented:
            self.assertIn(rule, commits.__doc__)

    def test_no_passing_fixture_names_the_worklog(self):
        # A message names nothing of the worklog, which the publication gate checks (tools/public/public_check.py):
        # an example this gate passes must pass that one too.
        sys.path.insert(0, str(HERE.parent / "public"))
        import public_check

        for path in sorted((commits.FIXTURES / "pass").glob("*.txt")):
            found = list(public_check.find_references(path.read_bytes(), public_check.MESSAGE_REFERENCES))
            self.assertEqual(found, [], path.name)


class Signoff(unittest.TestCase):
    # check_message's own default is off, for the fixtures; the gate's (main) is on, as the Repository tests show.
    def test_off_by_default(self):
        self.assertEqual(commits.check_message("docs: describe the pool settings\n"), [])

    def test_on_requires_the_last_footer(self):
        found = commits.check_message("docs: describe the pool settings\n", signoff=True)
        self.assertEqual([r for r, _ in found], ["signoff"])
        self.assertEqual(commits.check_message(GOOD, signoff=True), [])

    def test_on_requires_the_authors(self):
        found = commits.check_message(GOOD, signoff=True, author="John Roe <john@example.org>")
        self.assertEqual([r for r, _ in found], ["signoff"])
        self.assertEqual(commits.check_message(GOOD, signoff=True, author="Jane Doe <jane@example.org>"), [])


class Repository(unittest.TestCase):
    """A throwaway history: `old` before the base in the style of before, then the base, then the new commits."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = self.tmp.name
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Jane Doe")
        self.git("config", "user.email", "jane@example.org")
        self.git("config", "commit.gpgsign", "false")
        self.commit("Old style, before the public history\n")

    def tearDown(self):
        self.tmp.cleanup()

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.dir, capture_output=True, text=True, check=True).stdout.strip()

    def commit(self, message):
        self.git("commit", "-q", "--allow-empty", "--cleanup=verbatim", "-m", message)
        return self.git("rev-parse", "HEAD")

    def gate(self, *args, base=None, stdin="", cwd=None, signoff=None):
        env = {k: v for k, v in os.environ.items() if not k.startswith(("COMMITS_", "GIT_"))}
        env["COMMITS_BASE"] = base or "none"
        if signoff is not None:
            env["COMMITS_SIGNOFF"] = signoff
        return subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            cwd=cwd or self.dir,
            env=env,
            input=stdin,
            capture_output=True,
            text=True,
        )

    def test_no_base_checks_the_fixtures_alone(self):
        self.commit(BAD)
        r = self.gate()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("no base yet", r.stderr)

    def test_the_range_after_the_base(self):
        base = self.commit(GOOD)
        self.commit(GOOD)
        r = self.gate(base=base)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        bad = self.commit(BAD)
        r = self.gate(base=base)
        self.assertEqual(r.returncode, 1)
        self.assertIn(f"{bad[:12]}: header-form", r.stdout)
        self.assertEqual(len(r.stdout.splitlines()), 1, "the commits before the base are not checked")

    def test_the_gate_requires_the_sign_off_by_default(self):
        base = self.commit(GOOD)
        unsigned = self.commit("docs: describe the pool settings\n")
        r = self.gate(base=base)
        self.assertEqual(r.returncode, 1)
        self.assertEqual(len(r.stdout.splitlines()), 1, r.stdout)
        self.assertIn(f"{unsigned[:12]}: signoff: the last footer is the author's `Signed-off-by:`", r.stdout)
        self.assertEqual(self.gate(base=base, signoff="0").returncode, 0)

    def test_the_gate_requires_the_authors_sign_off(self):
        base = self.commit(GOOD)
        other = self.commit(GOOD.replace("Jane Doe <jane@example.org>", "John Roe <john@example.org>"))
        r = self.gate(base=base)
        self.assertEqual(r.returncode, 1)
        self.assertEqual(len(r.stdout.splitlines()), 1, r.stdout)
        self.assertIn(f"{other[:12]}: signoff: no `Signed-off-by: Jane Doe <jane@example.org>`", r.stdout)

    def test_a_message_requires_the_sign_off_by_default(self):
        path = Path(self.dir) / "message.txt"
        path.write_text("docs: describe the pool settings\n")
        r = self.gate("--message", str(path))
        self.assertEqual(r.returncode, 1)
        self.assertIn(": signoff: ", r.stdout)
        self.assertEqual(self.gate("--message", str(path), signoff="0").returncode, 0)
        path.write_text(GOOD)
        r = self.gate("--message", str(path))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_a_root_base_is_checked_too(self):
        self.git("checkout", "-q", "--orphan", "public")
        root = self.commit(BAD)
        self.commit(GOOD)
        r = self.gate(base=root)
        self.assertEqual(r.returncode, 1)
        self.assertIn(f"{root[:12]}: header-form", r.stdout)

    def test_a_merge_commit_fails(self):
        base = self.commit(GOOD)
        self.git("checkout", "-q", "-b", "side")
        self.commit(GOOD.replace("pool", "chart"))
        self.git("checkout", "-q", "main")
        self.commit(GOOD)
        self.git("merge", "-q", "--no-ff", "-m", GOOD, "side")
        r = self.gate(base=base)
        self.assertEqual(r.returncode, 1)
        self.assertIn(": merge:", r.stdout)

    def test_an_unknown_base_is_a_usage_error(self):
        r = self.gate(base="0123456789abcdef0123456789abcdef01234567")
        self.assertEqual(r.returncode, 2)

    def shallow_clone(self):
        """A `--depth=1` clone of this history, as a CI runner makes one: the base is not in it."""
        clone = tempfile.TemporaryDirectory()
        self.addCleanup(clone.cleanup)
        subprocess.run(
            ["git", "clone", "-q", "--depth=1", f"file://{self.dir}", clone.name], capture_output=True, check=True
        )
        return clone.name

    def test_a_shallow_clone_is_deepened_to_reach_the_base(self):
        base = self.commit(GOOD)
        self.commit(GOOD)
        self.commit(GOOD)
        clone = self.shallow_clone()
        r = self.gate(base=base, cwd=clone)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("2 commits after", r.stderr)

    def test_a_failed_deepening_says_why(self):
        base = self.commit(GOOD)
        self.commit(GOOD)
        clone = self.shallow_clone()
        gone = os.path.join(self.dir, "gone")
        subprocess.run(["git", "remote", "set-url", "origin", f"file://{gone}"], cwd=clone, check=True)
        r = self.gate(base=base, cwd=clone)
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        # git's own reason, not only that the base is missing.
        self.assertIn("gone", r.stderr)
        self.assertIn("fetch", r.stderr)

    def test_pre_push_checks_the_pushed_commits_after_the_base(self):
        base = self.commit(GOOD)
        pushed = self.commit(GOOD)
        top = self.commit(BAD)
        line = f"refs/heads/main {top} refs/heads/main {pushed}\n"
        r = self.gate("--pre-push", "origin", base=base, stdin=line)
        self.assertEqual(r.returncode, 1)
        self.assertEqual(
            r.stdout.splitlines(),
            [f"{top[:12]}: header-form: `Describe the pool settings` is not `<type>[(<scope>)][!]: <description>`"],
        )
        line = f"refs/heads/main {pushed} refs/heads/main {base}\n"
        self.assertEqual(self.gate("--pre-push", "origin", base=base, stdin=line).returncode, 0)

    def test_pre_push_of_a_new_branch_without_a_remote(self):
        old = self.git("rev-parse", "HEAD")
        base = self.commit(GOOD)
        top = self.commit(BAD)
        line = f"refs/heads/main {top} refs/heads/main {'0' * 40}\n"
        r = self.gate("--pre-push", "origin", base=base, stdin=line)
        self.assertEqual(r.returncode, 1)
        self.assertNotIn(old[:12], r.stdout)
        self.assertIn(top[:12], r.stdout)

    def test_pre_push_without_a_base_skips(self):
        top = self.commit(BAD)
        line = f"refs/heads/main {top} refs/heads/main {'0' * 40}\n"
        r = self.gate("--pre-push", "origin", stdin=line)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)


if __name__ == "__main__":
    unittest.main()
