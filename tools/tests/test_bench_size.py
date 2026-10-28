"""bench.py size's command line: `--typical` selects sizing.py's typical fit, and `size hardware` prints
the /tmp it sizes."""

import json
import os
import subprocess
import sys
import unittest

BENCH = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "bench")
sys.path.insert(0, BENCH)

import sizing  # noqa: E402


def size(*args):
    """bench.py size's stdout; the run must succeed."""
    done = subprocess.run(
        [sys.executable, os.path.join(BENCH, "bench.py"), "size", *args], capture_output=True, text=True
    )
    if done.returncode != 0:
        raise AssertionError(f"exit {done.returncode}: {done.stdout}{done.stderr}")
    return done.stdout


class Typical(unittest.TestCase):
    def setUp(self):
        self.k = sizing.load(None)

    def test_hardware_defaults_to_the_worst_case(self):
        answer = json.loads(size("hardware", "--cpus", "4", "--memory", "8", "--json"))
        self.assertEqual((answer["basis"], answer["pool"]), ("worst", 3))

    def test_hardware_typical_selects_the_typical_fit(self):
        answer = json.loads(size("hardware", "--cpus", "4", "--memory", "8", "--typical", "--json"))
        self.assertEqual(answer, json.loads(json.dumps(sizing.hardware(self.k, 4, 8, basis="typical"))))
        self.assertEqual((answer["basis"], answer["pool"], answer["bound"]), ("typical", 4, "CPU"))
        text = size("hardware", "--cpus", "4", "--memory", "8", "--typical")
        self.assertIn(f"memory limit {answer['memory_limit_gib']:.2f} GiB", text)
        self.assertIn("typical", text)

    def test_target_typical_sizes_the_pods_from_the_typical_fit(self):
        rows = json.loads(size("target", "--latency", "5", "--rate", "60/min", "--typical", "--json"))
        worst = json.loads(size("target", "--latency", "5", "--rate", "60/min", "--json"))
        sized = [r for r in rows if r["pods"]]
        self.assertTrue(sized)
        for row in sized:
            self.assertAlmostEqual(row["memory_gib"], sizing.memory_needed(self.k, row["pool"], basis="typical") / 1024)
        self.assertLess(sized[0]["memory_gib"], next(r for r in worst if r["pods"])["memory_gib"])


class Tmp(unittest.TestCase):
    def setUp(self):
        self.k = sizing.load(None)

    def test_hardware_prints_the_worst_case_tmp_on_disk(self):
        tmp = sizing.hardware(self.k, 4, 8)["tmp_mib"]
        self.assertEqual(tmp, sizing.tmp_worst_mib(self.k, 3, 3))
        self.assertIn(f"/tmp {tmp} MiB (worst case)", size("hardware", "--cpus", "4", "--memory", "8"))

    def test_hardware_prints_the_tmpfs_it_counts_in_memory(self):
        tmp = sizing.hardware(self.k, 2, 16, tmpfs=True)["tmp_mib"]
        self.assertIn(
            f"/tmp {tmp} MiB (worst case), a tmpfs", size("hardware", "--cpus", "2", "--memory", "16", "--tmpfs")
        )

    def test_typical_names_its_tmp_as_typical(self):
        tmp = sizing.hardware(self.k, 4, 8, basis="typical")["tmp_mib"]
        self.assertIn(f"/tmp {tmp} MiB (typical mix)", size("hardware", "--cpus", "4", "--memory", "8", "--typical"))


if __name__ == "__main__":
    unittest.main()
