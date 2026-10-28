"""bench.py probe's plan and numbers: the busy phase keeps every instance converting."""

import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "bench"))

import bench  # noqa: E402


def summary(vus, throughput, mean, cpu, per=None, wait=None, conversion=None):
    """The fields of analyze.py's summary.json that probe_numbers reads."""
    return {
        "vus": vus,
        "throughput": throughput,
        "latency": {"mean": mean},
        "cpu_utilisation": cpu,
        "cpu_seconds_per_conversion": per,
        "server_wait_mean_s": wait,
        "server_conversion_mean_s": conversion,
    }


class Plan(unittest.TestCase):
    def test_busy_runs_more_clients_than_instances(self):
        for cpus in (1, 2, 4, 8):
            plan = bench.probe_plan(cpus)
            self.assertEqual(plan["alone"]["vus"], 1)
            self.assertEqual(plan["busy"]["vus"], cpus + 2, "a queue of two: no instance waits for a transfer")
            self.assertEqual(plan["busy"]["conversions"], 120 + 3 * cpus, "the endpoint's default busy count")
            self.assertEqual(plan["alone"]["conversions"], 20)


class Numbers(unittest.TestCase):
    # 0071, cloud VM, pool 4 on 4 vCPU, pair 2: alone, and the busy cell with 4 clients (no queue).
    ALONE = summary(1, 1.072, 0.887, 0.226, per=0.859, conversion=0.847)
    UNQUEUED = summary(4, 3.923, 0.992, 0.848, per=0.9, wait=6.5e-07, conversion=0.91)
    # 0078, the same VM, pair 1: the busy cell with 6 clients (a queue).
    QUEUED = summary(6, 4.6658, 1.258, 0.8774, per=0.7979, wait=0.3318, conversion=0.8367)

    def test_s_sat_is_pool_over_throughput(self):
        n = bench.probe_numbers(4, self.ALONE, self.QUEUED)
        self.assertAlmostEqual(n["s_sat"], 4 / 4.6658)
        self.assertAlmostEqual(n["s1"], 0.887)
        self.assertAlmostEqual(n["contention"], 4 / 4.6658 / 0.887)
        self.assertEqual(n["warnings"], [])
        self.assertEqual(
            n["busy"],
            {
                "vus": 6,
                "occupancy": 0.976,
                "cpu_steady": 0.931,
                "cpu_utilisation": 0.877,
                "server_wait_mean_s": 0.332,
                "server_conversion_mean_s": 0.837,
            },
        )

    def test_unqueued_busy_run_warns(self):
        n = bench.probe_numbers(4, self.ALONE, self.UNQUEUED)
        self.assertEqual(len(n["warnings"]), 2)
        self.assertIn("no queue", n["warnings"][0])
        self.assertIn("converted 0.89 of the time", n["warnings"][1])

    def test_queued_but_idle_warns(self):
        n = bench.probe_numbers(4, self.ALONE, summary(6, 3.5, 1.6, 0.8, per=0.9, conversion=1.0))
        self.assertEqual(len(n["warnings"]), 1)
        self.assertIn("idled", n["warnings"][0])

    def test_no_server_metrics_no_occupancy_check(self):
        # moalhaddar/docx-to-pdf and gotenberg have no /metrics: nothing to check the occupancy with.
        n = bench.probe_numbers(4, self.ALONE, summary(6, 3.5, 1.6, 0.8))
        self.assertEqual(n["warnings"], [])
        self.assertIsNone(n["busy"]["occupancy"])


if __name__ == "__main__":
    unittest.main()
