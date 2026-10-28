"""Tests of tools/public/public_check.py through its command line, on throwaway git
repositories with fixture denylists. The private names here are invented; the real denylist is never read.

Run: python3 -m unittest discover -s tools/public   (part of `make tools-test`)
"""

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

TOOL = Path(__file__).resolve().parent / "public_check.py"
HOOK = Path(__file__).resolve().parent / "pre-push"
COMMITS = Path(__file__).resolve().parents[1] / "lint" / "commits.py"
MAKEFILE = Path(__file__).resolve().parents[2] / "Makefile"
ZERO = "0" * 40


def planted(text):
    """A planted local path or internal host, written with `|` inside so that this tracked file names none itself."""
    return text.replace("|", "")


def git(cwd, *args, stdin=None):
    env = dict(
        os.environ,
        GIT_AUTHOR_NAME="A",
        GIT_AUTHOR_EMAIL="a@example.com",
        GIT_COMMITTER_NAME="A",
        GIT_COMMITTER_EMAIL="a@example.com",
        GIT_CONFIG_GLOBAL=os.devnull,
        GIT_CONFIG_NOSYSTEM="1",
    )
    out = subprocess.run(["git", *args], cwd=cwd, env=env, input=stdin, capture_output=True, text=True)
    if out.returncode != 0:
        raise AssertionError(f"git {args}: {out.stderr}")
    return out.stdout.strip()


class Repo:
    """A git repository in a temporary directory, one commit per `commit` call."""

    def __init__(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup = None
        self.base = Path(self.tmp.name)
        self.root = self.base / "repo"
        self.root.mkdir()
        git(self.root, "init", "-q", "-b", "main")
        self.denylist = self.base / "denylist"
        self.denylist.write_text("# fixture names\n\nacme-corp\nwidget\\s*cloud\n")

    def write(self, path, text):
        p = self.root / path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)

    def commit(self, message="a change", **files):
        for path, text in files.items():
            self.write(path, text)
        git(self.root, "add", "-A")
        git(self.root, "commit", "-q", "--allow-empty", "-m", message)
        return git(self.root, "rev-parse", "HEAD")

    def check(self, *args, denylist=None, stdin=None):
        env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        env["PUBLIC_DENYLIST"] = str(self.denylist) if denylist is None else denylist
        return subprocess.run(
            [sys.executable, str(TOOL), *args], cwd=self.root, env=env, input=stdin, capture_output=True, text=True
        )


class Case(unittest.TestCase):
    def repo(self):
        r = Repo()
        self.addCleanup(r.tmp.cleanup)
        return r

    def assertFails(self, out, *needles):
        self.assertEqual(out.returncode, 1, out.stdout + out.stderr)
        for n in needles:
            self.assertIn(n, out.stdout)

    def assertPasses(self, out):
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)


class TheTree(Case):
    def test_a_clean_tree_passes(self):
        r = self.repo()
        r.commit(**{"README.md": "A service.\n", "src/main.rs": "fn main() {}\n"})
        self.assertPasses(r.check())

    def test_a_private_name_fails_naming_the_file_the_line_and_the_pattern(self):
        r = self.repo()
        r.commit(**{"docs/a.md": "one\nDeployed by ACME-Corp's chart.\n"})
        self.assertFails(r.check(), "docs/a.md:2:", "acme-corp")

    def test_every_planted_name_is_reported_not_only_the_first(self):
        r = self.repo()
        r.commit(**{"a.md": "acme-corp\n", "b/c.txt": "x\n\nran on Widget Cloud\n"})
        self.assertFails(r.check(), "a.md:1:", "b/c.txt:3:", "widget\\s*cloud")

    def test_a_local_path_fails_without_a_denylist(self):
        r = self.repo()
        r.commit(
            **{
                "a.md": planted("see /Us|ers/jdoe/Projects/x\n"),
                "b.sh": planted("cd ~/Proj|ects/sibling\n"),
                "c.rs": planted("// ~/docx|2pdf/fixtures\n"),
                "d.md": planted("/ho|me/jdoe/src\n"),
            }
        )
        self.assertFails(r.check(denylist="none"), "a.md:1:", "b.sh:1:", "c.rs:1:", "d.md:1:")

    def test_an_internal_host_fails_without_a_denylist(self):
        r = self.repo()
        r.commit(
            **{
                "a.yml": planted("url: http://git.loc|alhost/x\n"),
                "b.md": planted("stand.inter|nal\n"),
                "c.md": planted("printer.lo|cal\n"),
            }
        )
        self.assertFails(r.check(denylist="none"), "a.yml:1:", "b.md:1:", "c.md:1:")

    def test_a_private_range_address_is_left_to_the_denylist(self):
        r = self.repo()
        r.commit(**{"a.rs": 'let link = "tcp://10.96.0.5:8080";\n', "b.md": "the stand at 10.20.30.40\n"})
        self.assertPasses(r.check(denylist="none"))
        r.denylist.write_text(r"\b10\.20\.30\.\d+" + "\n")
        self.assertFails(r.check(), "b.md:1:")

    def test_generic_placeholders_and_well_known_names_pass(self):
        r = self.repo()
        r.commit(
            **{
                "a.md": "paths like `/Users/…`, `/Users/<you>/x`, `/home/<user>/…`, `~/…`, `~/Projects/…`\n"
                "the default `~/.config/oxidestack/public-denylist`\n"
                "loopback 127.0.0.1, localhost:8080, host.docker.internal, x.ns.svc.cluster.local\n"
                "a version 1.10.2.3 and self.local_addr()\n"
            }
        )
        self.assertPasses(r.check(denylist="none"))

    def test_only_tracked_files_are_read(self):
        r = self.repo()
        r.commit(**{"a.md": "clean\n", ".gitignore": "ignored/\n"})
        r.write("ignored/x.md", "acme-corp\n")
        r.write("untracked.md", "acme-corp\n")
        self.assertPasses(r.check())

    def test_the_working_copy_of_a_tracked_file_is_read(self):
        r = self.repo()
        r.commit(**{"a.md": "clean\n"})
        r.write("a.md", "now acme-corp\n")
        self.assertFails(r.check(), "a.md:1:")

    def test_a_submodule_is_not_entered(self):
        sub = self.repo()
        sub.commit(**{"x.md": planted("/Us|ers/someone/else\n")})
        r = self.repo()
        git(r.root, "-c", "protocol.file.allow=always", "submodule", "add", "-q", str(sub.root), "up")
        r.commit(**{"a.md": "clean\n"})
        self.assertPasses(r.check(denylist="none"))

    def test_a_name_inside_a_binary_file_is_found(self):
        r = self.repo()
        (r.root / "g.pdf").write_bytes(b"%PDF\x00\x01 /Creator (acme-corp) \x00\n")
        r.commit()
        self.assertFails(r.check(), "g.pdf:")


class ThePaths(Case):
    """A tracked file's path is matched as its contents are: a name in a directory or a file name is published too."""

    def test_a_private_name_in_a_tracked_path_fails_naming_the_path(self):
        r = self.repo()
        r.commit(**{"ci/acme-corp/jobs.yml": "clean\n", "docs/Widget Cloud.md": "clean\n"})
        self.assertFails(r.check(), "ci/acme-corp/jobs.yml: path: acme-corp", "docs/Widget Cloud.md: path:")

    def test_a_built_in_pattern_matches_a_path_without_a_denylist(self):
        r = self.repo()
        r.commit(**{planted("hosts/stand.inter|nal.yml"): "clean\n"})
        self.assertFails(r.check(denylist="none"), planted("hosts/stand.inter|nal.yml: path:"))

    def test_a_clean_path_passes_and_an_untracked_one_is_not_read(self):
        r = self.repo()
        r.commit(**{"src/main.rs": "fn main() {}\n", ".gitignore": "ignored/\n"})
        r.write("ignored/acme-corp.md", "clean\n")
        r.write("acme-corp.md", "clean\n")
        self.assertPasses(r.check())

    def test_a_pushed_commit_s_paths_are_checked(self):
        r = self.repo()
        bare = r.base / "bare.git"
        git(r.root, "init", "-q", "--bare", str(bare))
        git(r.root, "remote", "add", "origin", str(bare))
        first = r.commit("first", **{"a.md": "clean\n"})
        git(r.root, "push", "-q", "origin", "main")
        sha = r.commit("second", **{"acme-corp/b.md": "clean\n"})
        out = r.check("--pre-push", "origin", stdin=f"refs/heads/main {sha} refs/heads/main {first}\n")
        self.assertFails(out, "acme-corp/b.md: path: acme-corp")


class TheDenylist(Case):
    def test_a_missing_denylist_fails_rather_than_passing(self):
        r = self.repo()
        r.commit(**{"a.md": "clean\n"})
        out = r.check(denylist=str(r.base / "nowhere"))
        self.assertEqual(out.returncode, 2, out.stdout + out.stderr)
        self.assertIn("nowhere", out.stderr)
        self.assertIn("PUBLIC_DENYLIST=none", out.stderr)

    def test_a_denylist_of_only_comments_fails(self):
        r = self.repo()
        r.commit(**{"a.md": "clean\n"})
        r.denylist.write_text("# nothing yet\n\n")
        self.assertEqual(r.check().returncode, 2)

    def test_an_invalid_pattern_fails_naming_its_line(self):
        r = self.repo()
        r.commit(**{"a.md": "clean\n"})
        r.denylist.write_text("ok\n(unclosed\n")
        out = r.check()
        self.assertEqual(out.returncode, 2, out.stdout + out.stderr)
        self.assertIn(":2", out.stderr)

    def test_none_runs_only_the_built_in_patterns(self):
        r = self.repo()
        r.commit(**{"a.md": "acme-corp\n"})
        self.assertPasses(r.check(denylist="none"))
        self.assertFails(r.check(), "a.md:1:")

    def test_the_default_denylist_is_under_the_users_config(self):
        r = self.repo()
        r.commit(**{"a.md": "clean\n"})
        home = r.base / "home"
        env = dict(os.environ, HOME=str(home), GIT_CONFIG_GLOBAL=os.devnull)
        env.pop("PUBLIC_DENYLIST", None)
        out = subprocess.run([sys.executable, str(TOOL)], cwd=r.root, env=env, capture_output=True, text=True)
        self.assertEqual(out.returncode, 2)
        self.assertIn(".config/oxidestack/public-denylist", out.stderr)
        (home / ".config/oxidestack").mkdir(parents=True)
        (home / ".config/oxidestack/public-denylist").write_text("acme-corp\n")
        out = subprocess.run([sys.executable, str(TOOL)], cwd=r.root, env=env, capture_output=True, text=True)
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)

    def test_a_denylist_in_any_other_directory_of_the_users_config_is_not_read(self):
        r = self.repo()
        r.commit(**{"a.md": "clean\n"})
        home = r.base / "home"
        env = dict(os.environ, HOME=str(home), GIT_CONFIG_GLOBAL=os.devnull)
        env.pop("PUBLIC_DENYLIST", None)
        other = home / ".config" / "another-organisation"
        other.mkdir(parents=True)
        (other / "public-denylist").write_text("acme-corp\n")
        out = subprocess.run([sys.executable, str(TOOL)], cwd=r.root, env=env, capture_output=True, text=True)
        self.assertEqual(out.returncode, 2, out.stdout + out.stderr)
        self.assertIn(".config/oxidestack/public-denylist", out.stderr)


class TheMessages(Case):
    def test_an_unpushed_commit_message_is_checked(self):
        r = self.repo()
        sha = r.commit("Measured on the acme-corp stand", **{"a.md": "clean\n"})
        self.assertFails(r.check(), f"commit {sha[:12]}", "acme-corp")

    def test_a_message_already_on_a_remote_is_not(self):
        remote = self.repo()
        git(remote.root, "init", "-q", "--bare", str(remote.base / "bare.git"))
        r = self.repo()
        r.commit("Measured on the acme-corp stand", **{"a.md": "clean\n"})
        git(r.root, "remote", "add", "private", str(remote.base / "bare.git"))
        git(r.root, "push", "-q", "private", "main")
        r.commit("a clean one")
        self.assertPasses(r.check())


class TheWorklogRule(Case):
    """The worklog never reaches the code repository: with PUBLIC_WORKLOG=on a worklog path, or a ticket, decision
    record or research reference in a tracked file or a checked commit message, fails. Every reference here is planted
    with `|` inside, so that this tracked file names none itself."""

    def check(self, r, *args, rule="on", stdin=None):
        env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1", PUBLIC_DENYLIST="none")
        if rule is None:
            env.pop("PUBLIC_WORKLOG", None)
        else:
            env["PUBLIC_WORKLOG"] = rule
        return subprocess.run(
            [sys.executable, str(TOOL), *args], cwd=r.root, env=env, input=stdin, capture_output=True, text=True
        )

    def leaky(self):
        """A repository with the acceptance criteria's three leaks: a reference in a comment, a tracked decision record
        and a commit message in the worklog's convention."""
        r = self.repo()
        sha = r.commit(
            planted("Land 01|40: the pool"),
            **{
                "src/a.rs": planted("// see tick|et 0140 for why\nfn main() {}\n"),
                planted("docs/ad|r/x.md"): "a decision\n",
            },
        )
        return r, sha

    def test_on_by_default_and_off_on_request(self):
        r, _ = self.leaky()
        self.assertFails(self.check(r, rule=None), planted("docs/ad|r/x.md: worklog path"))
        self.assertPasses(self.check(r, rule="off"))
        self.assertIn("worklog rule off", self.check(r, rule="off").stderr)

    def test_on_fails_a_reference_in_a_comment_a_tracked_decision_record_and_a_commit_message(self):
        r, sha = self.leaky()
        self.assertFails(
            self.check(r),
            planted("src/a.rs:1: worklog reference: tick|et 0140"),
            planted("docs/ad|r/x.md: worklog path"),
            f"commit {sha[:12]}: line 1: worklog reference: " + planted("Land 01|40"),
        )

    def test_a_bad_value_is_unusable(self):
        r = self.repo()
        r.commit(**{"a.md": "clean\n"})
        out = self.check(r, rule="yes")
        self.assertEqual(out.returncode, 2, out.stdout + out.stderr)
        self.assertIn("PUBLIC_WORKLOG", out.stderr)

    def test_every_worklog_path_fails(self):
        r = self.repo()
        paths = [
            planted(p)
            for p in (
                "tick|ets/0001-x.md",
                "docs/tr|ee.md",
                "docs/mile|stones.md",
                "docs/res|earch/0001-x.md",
                "docs/age|nts/domain.md",
                "CLA|UDE.md",
                "tests/scenarios/CLA|UDE.md",
                ".cla|ude/skills/next/SKILL.md",
                "CON|TEXT.md",
                "docs/diver|gences.md",
                "docs/posi|tioning.md",
                "proto|types/x/main.rs",
                "tools/ticket_|tree.py",
                "tools/tests/test_ticket_|tree.py",
            )
        ]
        r.commit(**{p: "clean\n" for p in paths})
        out = self.check(r)
        self.assertFails(out, *[f"{p}: worklog path" for p in paths])

    def test_the_reference_shapes_fail_whatever_their_case(self):
        r = self.repo()
        lines = [
            "Tick|et 0082: the limit",
            "as tick|ets 0014 and 0016 found",
            "00|91's answer",
            "00|91’s answer",
            "AD|R 0008 §8",
            "adr-00|05",
            "rese|arch 0014",
            "research no|te 0021",
            "Clo|se 01|63-0166",
            "bran|ch 0158",
            "dispatch it|em 0162",
            "Filed 01|59 from the review",
            "landing 00|28b",
            "see docs/ad|r/0008-x.md",
            "the tree in docs/tr|ee.md",
            "the vocabulary of CONT|EXT.md",
        ]
        r.commit(**{"a.md": "".join(planted(line) + "\n" for line in lines)})
        out = self.check(r)
        self.assertFails(out, *[f"a.md:{n}: worklog reference" for n in range(1, len(lines) + 1)])

    def test_a_reference_split_across_lines_fails_naming_its_first_line(self):
        r = self.repo()
        r.commit(
            **{
                "a.sh": planted("x=1\n# the links job, once tick|et\n\t\t# 0110 lands\n"),
                "b.rs": planted("/// as the\n/// rese|arch\n/// 0014 measured\n"),
                "c.md": planted("tick|ets\n0014 and 0016\n"),
            }
        )
        self.assertFails(self.check(r), "a.sh:2:", "b.rs:2:", "c.md:1:")

    def test_numbers_and_words_that_are_not_references_pass(self):
        r = self.repo()
        r.commit(
            **{
                "a.md": "port 8080, mode 0644, the 1990's, a ticketing system 0140, version 1.0.0140\n"
                "a hash 0140abc, research shows, an ADR, the closed 2048 sockets, 0x0140\n"
                "the docs/guide/ pages and a CONTEXT section\n"
            }
        )
        self.assertPasses(self.check(r))

    def test_a_commit_message_is_held_stricter_any_ticket_number_alone_fails(self):
        r = self.repo()
        a = r.commit(planted("01|65: tools say their reasons in place"))
        b = r.commit(planted("Drop 00|22 from the plan"))
        c = r.commit("The pool restarts a hung converter (port 8080, version 1.0.2)")
        out = self.check(r)
        self.assertFails(out, f"commit {a[:12]}", f"commit {b[:12]}")
        self.assertNotIn(f"commit {c[:12]}", out.stdout)

    def test_a_number_alone_in_a_file_is_not_a_reference(self):
        r = self.repo()
        r.commit(**{"a.json": '{"id": 0140, "pages": [0001, 0002]}\n'})
        self.assertPasses(self.check(r))

    def test_the_pre_push_mode_checks_the_pushed_tree_and_messages(self):
        r = self.repo()
        bare = r.base / "bare.git"
        git(r.root, "init", "-q", "--bare", str(bare))
        git(r.root, "remote", "add", "origin", str(bare))
        first = r.commit("first", **{"a.md": "clean\n"})
        git(r.root, "push", "-q", "origin", "main")
        sha = r.commit(planted("Clo|se 0140"), **{planted("tick|ets/0140-x.md"): "x\n"})
        line = f"refs/heads/main {sha} refs/heads/main {first}\n"
        self.assertPasses(self.check(r, "--pre-push", "origin", rule="off", stdin=line))
        self.assertFails(
            self.check(r, "--pre-push", "origin", stdin=line),
            planted("tick|ets/0140-x.md: worklog path"),
            f"commit {sha[:12]}",
        )

    def test_a_message_already_on_a_remote_is_not_checked(self):
        r = self.repo()
        bare = r.base / "bare.git"
        git(r.root, "init", "-q", "--bare", str(bare))
        git(r.root, "remote", "add", "origin", str(bare))
        r.commit(planted("Land 01|40: before the split"), **{"a.md": "clean\n"})
        git(r.root, "push", "-q", "origin", "main")
        r.commit("a code change")
        self.assertPasses(self.check(r))


class TheArchive(Case):
    def test_a_head_that_contains_the_archive_is_refused(self):
        r = self.repo()
        r.commit(**{"a.md": "clean\n"})
        git(r.root, "tag", "-a", "-m", "archive", "pre-publication")
        r.commit("later")
        self.assertFails(r.check(), "pre-publication")

    def test_a_fresh_history_beside_the_archive_passes(self):
        r = self.repo()
        r.commit(**{"a.md": "clean\n"})
        git(r.root, "tag", "-a", "-m", "archive", "pre-publication")
        git(r.root, "checkout", "-q", "--orphan", "fresh")
        r.commit("the cut")
        self.assertPasses(r.check())

    def test_the_archive_on_the_archive_remote_is_found_after_the_local_tag_is_deleted(self):
        remote = self.repo()
        bare = remote.base / "bare.git"
        git(remote.root, "init", "-q", "--bare", str(bare))
        r = self.repo()
        r.commit(**{"a.md": "clean\n"})
        git(r.root, "tag", "-a", "-m", "archive", "pre-publication")
        git(r.root, "remote", "add", "private", str(bare))
        git(r.root, "push", "-q", "private", "pre-publication")
        git(r.root, "tag", "-d", "pre-publication")
        r.commit("built on the archive")
        self.assertFails(r.check(), "pre-publication")


class ThePrePushMode(Case):
    """`--pre-push <remote>` reads git's pre-push lines: `<local ref> <local sha> <remote ref> <remote sha>`."""

    def setUp(self):
        self.r = self.repo()
        self.bare = self.r.base / "bare.git"
        git(self.r.root, "init", "-q", "--bare", str(self.bare))
        git(self.r.root, "remote", "add", "origin", str(self.bare))
        self.first = self.r.commit("first", **{"a.md": "clean\n"})
        git(self.r.root, "push", "-q", "origin", "main")

    def line(self, sha, remote_sha):
        return f"refs/heads/main {sha} refs/heads/main {remote_sha}\n"

    def test_the_pushed_commit_s_tree_is_checked_not_the_working_copy(self):
        sha = self.r.commit("second", **{"b.md": "acme-corp\n"})
        self.r.write("b.md", "fixed in the working copy only\n")
        out = self.r.check("--pre-push", "origin", stdin=self.line(sha, self.first))
        self.assertFails(out, "b.md:1:")

    def test_only_the_pushed_commits_messages_are_checked(self):
        git(self.r.root, "commit", "-q", "--allow-empty", "-m", "acme-corp is fine here")
        git(self.r.root, "push", "-q", "origin", "main")
        old = git(self.r.root, "rev-parse", "HEAD")
        sha = self.r.commit("a clean message")
        self.assertPasses(self.r.check("--pre-push", "origin", stdin=self.line(sha, old)))
        bad = self.r.commit("Widget Cloud numbers")
        self.assertFails(self.r.check("--pre-push", "origin", stdin=self.line(bad, old)), f"commit {bad[:12]}")

    def test_a_new_branch_checks_the_commits_no_remote_branch_has(self):
        git(self.r.root, "checkout", "-q", "-b", "topic")
        sha = self.r.commit("acme-corp leak")
        out = self.r.check("--pre-push", "origin", stdin=f"refs/heads/topic {sha} refs/heads/topic {ZERO}\n")
        self.assertFails(out, f"commit {sha[:12]}")
        self.assertNotIn(f"commit {self.first[:12]}", out.stdout)

    def test_a_push_of_the_archive_is_refused(self):
        git(self.r.root, "tag", "-a", "-m", "archive", "pre-publication")
        sha = self.r.commit("on top")
        out = self.r.check("--pre-push", "origin", stdin=self.line(sha, self.first))
        self.assertFails(out, "refs/heads/main", "pre-publication")

    def test_a_deletion_is_not_checked(self):
        out = self.r.check("--pre-push", "origin", stdin=f"(delete) {ZERO} refs/heads/old {self.first}\n")
        self.assertPasses(out)


class TheHook(Case):
    def test_make_hooks_installs_a_pre_push_hook_that_runs_both_gates(self):
        r = self.repo()
        r.commit(**{"a.md": "clean\n"})
        out = subprocess.run(
            [
                "make",
                "-s",
                "-f",
                str(MAKEFILE),
                "-C",
                str(MAKEFILE.parent),
                "hooks",
                f"HOOKS_DIR={r.root / '.git' / 'hooks'}",
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        hook = r.root / ".git/hooks/pre-push"
        self.assertTrue(os.access(hook, os.X_OK))
        self.assertEqual(hook.read_text(), HOOK.read_text())
        text = HOOK.read_text()
        self.assertIn("make check", text)
        self.assertIn("make public-check", text)
        self.assertIn("make commits-check", text)

    def hooked(self, r, message="a change"):
        """A stand-in repository with the hook installed and a bare remote: a Makefile whose check passes and whose
        public-check and commits-check are the real tools, copied out of this checkout so that the fixture's own text
        names no local path. The environment to push with."""
        tool = shutil.copy(TOOL, r.base / "public_check.py")
        commits = shutil.copy(COMMITS, r.base / "commits.py")
        r.commit(
            message,
            **{
                "a.md": "clean\n",
                "Makefile": (
                    f"check:\n\t@true\npublic-check:\n\t@python3 {tool} $(if $(PUSH),--pre-push $(PUSH))\n"
                    f"commits-check:\n\t@python3 {commits} --pre-push $(PUSH)\n"
                ),
            },
        )
        bare = r.base / "bare.git"
        git(r.root, "init", "-q", "--bare", str(bare))
        git(r.root, "remote", "add", "origin", str(bare))
        hooks = r.root / ".git/hooks"
        hooks.mkdir(exist_ok=True)
        shutil.copy(HOOK, hooks / "pre-push")
        os.chmod(hooks / "pre-push", 0o755)
        return dict(os.environ, PUBLIC_DENYLIST=str(r.denylist), GIT_CONFIG_GLOBAL=os.devnull, COMMITS_BASE="none")

    def push(self, r, env):
        return subprocess.run(
            ["git", "push", "-q", "origin", "main"], cwd=r.root, env=env, capture_output=True, text=True
        )

    def test_the_hook_refuses_a_push_with_a_private_name(self):
        r = self.repo()
        env = self.hooked(r)
        ok = self.push(r, env)
        self.assertEqual(ok.returncode, 0, ok.stdout + ok.stderr)
        r.commit("acme-corp leak")
        bad = self.push(r, env)
        self.assertNotEqual(bad.returncode, 0)
        self.assertIn("acme-corp", bad.stdout + bad.stderr)

    def test_the_hook_refuses_a_commit_message_off_the_convention_after_the_base(self):
        r = self.repo()
        # The base is the root commit, so it is checked too: it is in the convention's form.
        env = self.hooked(r, "chore: start the public history")
        env["COMMITS_BASE"] = git(r.root, "rev-parse", "HEAD")
        ok = self.push(r, env)
        self.assertEqual(ok.returncode, 0, ok.stdout + ok.stderr)
        r.commit("docs: describe the pool settings")
        ok = self.push(r, env)
        self.assertEqual(ok.returncode, 0, ok.stdout + ok.stderr)
        r.commit("Describe the pool settings")
        bad = self.push(r, env)
        self.assertNotEqual(bad.returncode, 0)
        self.assertIn("header-form", bad.stdout + bad.stderr)


if __name__ == "__main__":
    unittest.main()
