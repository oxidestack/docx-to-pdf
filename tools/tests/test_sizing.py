"""sizing.py's launch settings: the conversion timeout never goes under the service's default; pod memory and /tmp
from the worst case."""

import math
import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "bench"))

import sizing  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def conversion_timeout(s1, cpus):
    k = sizing.load(None, s1=s1, contention=1.2)
    return sizing.settings(k, cpus, cpus)["DOCX_TO_PDF_CONVERSION_TIMEOUT"]


class ConversionTimeout(unittest.TestCase):
    def test_never_under_the_default(self):
        # The typical mix's 4 × p99 is a few seconds; tables-1000 takes 63 s.
        for s1, cpus in ((0.459, 1), (0.9, 4), (2.542, 8), (10.0, 2)):
            self.assertEqual(conversion_timeout(s1, cpus), "180s", (s1, cpus))

    def test_a_slower_tail_still_sets_it(self):
        seconds = int(conversion_timeout(60.0, 2).rstrip("s"))
        self.assertGreater(seconds, 180)
        self.assertEqual(seconds % 10, 0)

    def test_the_floor_is_the_services_default(self):
        with open(os.path.join(ROOT, "src", "config.rs")) as f:
            config = f.read()
        self.assertIn(
            f"DEFAULT_CONVERSION_TIMEOUT: Duration = Duration::from_secs({sizing.DEFAULT_CONVERSION_TIMEOUT_S});",
            config,
        )


class WorstCaseMemory(unittest.TestCase):
    """No instance passes the hard limit by more than one poll's growth, so a pod sized service +
    POOL_SIZE × (hard limit + overshoot) holds every instance at the limit at once, whatever documents come."""

    def setUp(self):
        self.k = sizing.load(None)

    def test_the_worst_case_is_the_default(self):
        self.assertEqual(sizing.memory_needed(self.k, 4), 256 + 4 * (2048 + 256))
        self.assertEqual(sizing.memory_needed(self.k, 1), 256 + 2048 + 256)

    def test_the_typical_fit_stays_a_named_option(self):
        self.assertEqual(sizing.memory_needed(self.k, 4, basis="typical"), 1.25 * (256 + 256 * 4))
        with self.assertRaises(ValueError):
            sizing.memory_needed(self.k, 4, basis="median")

    def test_a_tmpfs_adds_its_size(self):
        self.assertEqual(sizing.memory_needed(self.k, 2, 300), 256 + 2 * 2304 + 300)

    def test_the_pool_a_limit_allows(self):
        self.assertEqual(sizing.pool_for_memory(self.k, 8192), 3)
        self.assertEqual(sizing.pool_for_memory(self.k, 256 + 4 * 2304), 4)
        self.assertEqual(sizing.pool_for_memory(self.k, 8192, basis="typical"), 24)

    def test_hardware_is_bound_by_memory_at_the_worst_case(self):
        answer = sizing.hardware(self.k, 4, 8)
        self.assertEqual((answer["pool"], answer["bound"]), (3, "memory"))
        self.assertAlmostEqual(answer["memory_limit_gib"], (256 + 3 * 2304) / 1024)
        typical = sizing.hardware(self.k, 4, 8, basis="typical")
        self.assertEqual((typical["pool"], typical["bound"]), (4, "CPU"))
        self.assertIn("error", sizing.hardware(self.k, 1, 2))

    def test_the_hard_limit_is_the_services_default(self):
        with open(os.path.join(ROOT, "src", "config.rs")) as f:
            config = f.read()
        self.assertEqual(sizing.HARD_MEMORY_MIB % 1024, 0)
        self.assertIn(
            f"DEFAULT_INSTANCE_HARD_MEMORY: u64 = {sizing.HARD_MEMORY_MIB // 1024} * 1024 * 1024 * 1024;", config
        )
        self.assertGreater(sizing.HARD_MEMORY_MIB, self.k["memory"]["instance_max_mib"])

    def test_the_settings_name_the_hard_limit(self):
        self.assertEqual(sizing.settings(self.k, 2, 2)["DOCX_TO_PDF_INSTANCE_HARD_MEMORY"], "2048MiB")


class WorstCaseTmp(unittest.TestCase):
    """A conversion holds at most 3 × its document (pictures) or a long layout's PDF
    twice (tables within the timeout, 256 MiB); a waiting request its document; an instance its profile."""

    def setUp(self):
        self.k = sizing.load(None)

    def test_at_the_reports_document_limit(self):
        # 32 MiB: 3 × 32 < 256, so the layout term; 5 profiles, 4 waiting documents.
        self.assertEqual(sizing.tmp_worst_mib(self.k, 4, 4), math.ceil(5 * 0.35 + 4 * 32 + 4 * 256))

    def test_large_documents_take_three_times_their_size(self):
        self.assertEqual(
            sizing.tmp_worst_mib(self.k, 2, 2, document_mib=1000), math.ceil(3 * 0.35 + 2 * 1000 + 2 * 3000)
        )

    def test_a_tmpfs_counts_it_in_memory(self):
        answer = sizing.hardware(self.k, 2, 16, tmpfs=True)
        tmp = sizing.tmp_worst_mib(self.k, 2, 2)
        self.assertEqual(answer["tmp_mib"], tmp)
        self.assertAlmostEqual(answer["memory_limit_gib"], (256 + 2 * 2304 + tmp) / 1024)
        self.assertEqual(sizing.hardware(self.k, 2, 16)["tmp_mib"], tmp)


class Explain(unittest.TestCase):
    def test_the_memory_formula_is_the_worst_case(self):
        k = sizing.load(None, s1=0.94, contention=1.18)
        formulas = {f["what"]: f for f in sizing.explain(k, 4)["formulas"]}
        self.assertIn("256 + 4 × (2048 + 256) = 9472 MiB", formulas["memory limit (= request)"]["here"])
        self.assertIn("1600 MiB", formulas["memory limit for the typical mix only (a named option)"]["here"])
        self.assertIn("/tmp size", " ".join(formulas))


if __name__ == "__main__":
    unittest.main()
