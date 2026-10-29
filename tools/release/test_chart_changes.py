"""`tools/ci/release.sh changes <version>`: the chart's `artifacthub.io/changes` (Artifact Hub's changelog of a chart
version), written by `release.sh chart` from CHANGELOG.md's `## [<version>]` section. Each entry is one change, its
kind the section's: Features added, Bug fixes fixed, Security security, the others (Action required, Performance,
Other changes) changed. The introduction and the commit's scope and hash are left out; a version without a section
has none. Needs only sh and awk.
"""

import os
import subprocess
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
RELEASE = os.path.join(ROOT, "tools", "ci", "release.sh")


def changes(version, changelog):
    done = subprocess.run(
        ["sh", RELEASE, "changes", version],
        capture_output=True,
        text=True,
        env={**os.environ, "CHANGELOG": changelog},
    )
    if done.returncode != 0:
        raise AssertionError(f"release.sh changes {version}: exit {done.returncode}: {done.stderr}")
    return done.stdout


class Changes(unittest.TestCase):
    def test_a_drafted_section_one_entry_per_change(self):
        # The draft git-cliff writes (expected-tagged.txt is the section as drafted).
        self.assertEqual(
            changes("2.0.0", os.path.join(HERE, "expected-tagged.txt")),
            "- kind: changed\n"
            '  description: "`pool.size` is no longer read. A deployment that sets it sets `DOCX_TO_PDF_POOL_SIZE`'
            ' (or `POOL_SIZE`) to the same value instead."\n'
            "- kind: added\n"
            '  description: "The chart takes `priorityClassName` for the pods."\n'
            "- kind: fixed\n"
            '  description: "A LibreOffice instance that dies while starting is replaced, so the service no longer'
            ' runs with one instance fewer than configured."\n'
            "- kind: fixed\n"
            '  description: "An upload over the size limit is answered 413 instead of 400."\n'
            "- kind: changed\n"
            '  description: "A document with large photographs converts faster and in less memory."\n'
            "- kind: changed\n"
            '  description: "The operations guide says how to add fonts without rebuilding the image."\n',
        )

    def test_an_edited_section_wrapped_quoted_and_without_hashes(self):
        self.assertEqual(
            changes("1.3.0", os.path.join(HERE, "changes-changelog.txt")),
            "- kind: changed\n"
            '  description: "`pool.size` is gone: set `DOCX_TO_PDF_POOL_SIZE` instead."\n'
            "- kind: added\n"
            '  description: "The chart takes `priorityClassName`, a \\"class\\" for the pods, and `extraEnv` #'
            ' values with a backslash \\\\ too."\n'
            "- kind: added\n"
            '  description: "Written by hand, without a hash."\n'
            "- kind: fixed\n"
            '  description: "An upload over the size limit is answered 413."\n'
            "- kind: security\n"
            '  description: "The image drops a library with a known CVE."\n'
            "- kind: changed\n"
            '  description: "Faster pictures."\n'
            "- kind: changed\n"
            '  description: "The guide says how to add fonts."\n',
        )

    def test_the_section_ends_at_the_next_release(self):
        self.assertEqual(
            changes("1.2.0", os.path.join(HERE, "changes-changelog.txt")),
            '- kind: added\n  description: "Not in 1.3.0."\n',
        )

    def test_a_pre_release_without_a_section_has_none(self):
        self.assertEqual(changes("1.4.0-rc.1", os.path.join(HERE, "changes-changelog.txt")), "")

    def test_the_first_release(self):
        # CHANGELOG.md's own 1.0.0: an introduction, then one wrapped entry.
        self.assertEqual(
            changes("1.0.0", os.path.join(ROOT, "CHANGELOG.md")),
            "- kind: changed\n"
            '  description: "An instance returns the memory a conversion freed to the system before it answers, so a'
            " pod converting picture-heavy documents sits about 50 MiB lower between conversions. Peak memory and the"
            ' sizing formula are unchanged."\n',
        )


if __name__ == "__main__":
    unittest.main()
