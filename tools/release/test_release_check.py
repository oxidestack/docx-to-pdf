"""`tools/ci/release.sh check <tag>`: a release whose CHANGELOG.md section has Action required needs its section in the
upgrade guide, `## Upgrading to <version>` in docs/guide/upgrading.md; a release without one, and a pre-release, do
not. The tree's own version is the tag's, so the chart and Cargo.toml agree. Needs only sh and awk.
"""

import os
import re
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
RELEASE = os.path.join(ROOT, "tools", "ci", "release.sh")

with open(os.path.join(ROOT, "Cargo.toml"), encoding="utf-8") as f:
    VERSION = re.search(r'^version = "(.*)"$', f.read(), re.M).group(1)


def check(tag, changelog, upgrading):
    with tempfile.TemporaryDirectory() as tmp:
        files = {}
        for name, text in (("CHANGELOG", changelog), ("UPGRADING", upgrading)):
            files[name] = os.path.join(tmp, name)
            with open(files[name], "w", encoding="utf-8") as f:
                f.write(text)
        env = {**os.environ, **files}
        return subprocess.run(["sh", RELEASE, "check", tag], capture_output=True, text=True, env=env)


SECTION = f"## [{VERSION}] - 2026-11-01\n\n### {{kind}}\n\n- an entry\n"
GUIDE = "# Upgrading\n\n## How to upgrade\n"


class UpgradeGuide(unittest.TestCase):
    def test_action_required_without_its_section_fails(self):
        done = check(f"v{VERSION}", SECTION.format(kind="Action required"), GUIDE)
        self.assertEqual(done.returncode, 1, done.stderr)
        self.assertIn(f"has no ## Upgrading to {VERSION}", done.stderr)

    def test_action_required_with_its_section_passes(self):
        done = check(f"v{VERSION}", SECTION.format(kind="Action required"), GUIDE + f"\n## Upgrading to {VERSION}\n")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(done.stdout.strip(), VERSION)

    def test_a_release_that_asks_nothing_needs_no_section(self):
        done = check(f"v{VERSION}", SECTION.format(kind="Bug fixes"), GUIDE)
        self.assertEqual(done.returncode, 0, done.stderr)

    def test_a_pre_release_needs_nothing(self):
        done = check(f"v{VERSION}-rc.1", "", "")
        self.assertEqual(done.returncode, 0, done.stderr)


if __name__ == "__main__":
    unittest.main()
