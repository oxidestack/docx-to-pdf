"""`tools/ci/release.sh notes <version> [<digest>]`: the GitHub release's body. CHANGELOG.md's section, then the Install
and Verify blocks naming quay.io, the version, the digest when given and the release workflow's identity at the tag.
The release files' Sigstore bundles are in the current bundle format, which cosign 2 rejects by default, so the
Verify block says which cosign reads them. Needs only sh and awk.
"""

import os
import subprocess
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
RELEASE = os.path.join(ROOT, "tools", "ci", "release.sh")
IDENTITY = "https://github.com/oxidestack/docx-to-pdf/.github/workflows/release.yml@refs/tags/v2.0.0"


def notes(*args):
    env = {**os.environ, "CHANGELOG": os.path.join(HERE, "expected-tagged.txt")}
    env.pop("GITHUB_REPOSITORY", None)
    done = subprocess.run(["sh", RELEASE, "notes", *args], capture_output=True, text=True, env=env)
    if done.returncode != 0:
        raise AssertionError(f"release.sh notes {' '.join(args)}: exit {done.returncode}: {done.stderr}")
    return done.stdout


class Body(unittest.TestCase):
    def test_the_section_then_install_and_verify(self):
        body = notes("2.0.0", "sha256:" + "0" * 64)
        self.assertTrue(body.startswith("### Action required\n"), body[:200])
        self.assertNotIn("[2.0.0]: https://", body)
        self.assertIn("docker run --rm -p 8080:8080 quay.io/oxidestack/docx-to-pdf:2.0.0\n", body)
        self.assertIn("(`sha256:" + "0" * 64 + "`)", body)
        self.assertLess(body.index("## Install"), body.index("## Verify"))

    def test_verify_blob_names_the_identity_and_the_cosign_that_reads_the_bundles(self):
        body = notes("2.0.0")
        verify = body[body.index("## Verify") :]
        self.assertIn(
            "cosign verify-blob docx-to-pdf-2.0.0.tgz --bundle docx-to-pdf-2.0.0.tgz.sigstore.json"
            f" --certificate-identity {IDENTITY}",
            verify,
        )
        self.assertIn("cosign 3 or later", verify)
        self.assertIn("`--new-bundle-format`", verify)
        # The sentence comes before the command it is about.
        self.assertLess(verify.index("cosign 3 or later"), verify.index("\ncosign verify-blob "))


if __name__ == "__main__":
    unittest.main()
