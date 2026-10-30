"""compare.py and the torture.py it drives, without Docker: the container's cgroup found through bench.cgroup_dir,
moalhaddar/docx-to-pdf's shorter cap for its linked pictures, and the linked-picture row.

    python3 -m unittest discover -s tools/compare
"""

import json
import os
import random
import sys
import tempfile
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "bench"))
sys.path.insert(0, HERE)

import bench  # noqa: E402
import compare  # noqa: E402
import torture  # noqa: E402

# moalhaddar/docx-to-pdf's slowest answer in its linked-picture run on record (the full mode on an amd64 VM,
# 2026-10-27): d-linked-image-x10--int.docx, ten pictures linked to an address that never answers.
SLOWEST_ANSWER_S = 100.15


def service(driver):
    """A torture.Service whose `docker run` printed the id abc123, on a Docker host with this cgroup driver."""
    with mock.patch.object(torture, "sh", return_value="abc123"), mock.patch.object(bench, "sh", return_value=driver):
        return torture.Service("ours", "docx-to-pdf-test", 18125)


class Cgroup(unittest.TestCase):
    def test_cgroupfs(self):
        self.assertEqual(service("cgroupfs").cgroup, f"{torture.CGROUP_ROOT}/docker/abc123")

    def test_systemd(self):
        self.assertEqual(service("systemd").cgroup, f"{torture.CGROUP_ROOT}/system.slice/docker-abc123.scope")

    def test_compare_leaves_it_to_torture(self):
        init = torture.Service.__init__
        try:
            compare.patch_torture("docx-to-pdf:test", "4g", "0")
            self.assertEqual(service("cgroupfs").cgroup, f"{torture.CGROUP_ROOT}/docker/abc123")
        finally:
            torture.Service.__init__ = init
            torture.SUBJECTS["ours"]["image"] = os.environ.get("IMAGE_OURS", "docx-to-pdf:torture")


class LinkedPictures(unittest.TestCase):
    def run_egress(self, subject, cap, requests, events):
        caps = []

        def fake(a):
            caps.append(a.cap)
            os.makedirs(os.path.join(a.out, "pdf"))
            with open(os.path.join(a.out, "listener.jsonl"), "w") as f:
                f.write(json.dumps({"t": 0, "event": "listening", "ports": [80, 81]}) + "\n")
                f.writelines(json.dumps({"t": 1, "event": "connect", "local": local}) + "\n" for local in events)
                f.write("a line that is not JSON\n")
            with open(os.path.join(a.out, "pdf", "results.jsonl"), "w") as f:
                f.writelines(json.dumps(r) + "\n" for r in requests)

        with tempfile.TemporaryDirectory() as out, mock.patch.object(torture, "egress", fake):
            row = compare.egress(out, subject, cap)
        self.assertEqual(caps, [cap])
        return row

    def test_the_cap_is_shorter_and_above_the_slowest_answer(self):
        self.assertLess(compare.LINKED_CAP_MOALHADDAR, compare.CAP)
        self.assertGreater(compare.LINKED_CAP_MOALHADDAR, 1.2 * SLOWEST_ANSWER_S)

    def test_the_cap_reaches_the_client(self):
        requests = [
            {"doc": "d-linked-image-x10--int.docx", "status": 200, "seconds": SLOWEST_ANSWER_S},
            {"doc": "o-ole-link--int.odt", "status": "TimeoutError: timed out", "seconds": 150.01},
        ]
        row = self.run_egress("moalhaddar", 150, requests, ["198.18.124.10:80", "10.231.124.10:81", "10.231.124.10:81"])
        self.assertEqual(row["result"]["cap_s"], 150)
        self.assertEqual(row["result"]["connections_by_address"], {"public": 1, "private": 2})
        self.assertEqual(
            row["cell"],
            "2 documents: 1 × 200, 1 × TimeoutError: timed out; connections the listener logged: 3 "
            "(1 public, 2 private); 150 s a document",
        )

    def test_the_others_keep_the_cap_on_record(self):
        row = self.run_egress("gotenberg", compare.CAP, [{"doc": "a.docx", "status": 200, "seconds": 0.2}], [])
        self.assertEqual(row["cell"], "1 documents: 1 × 200; connections the listener logged: 0")

    def test_the_statuses_do_not_depend_on_the_cap(self):
        # A document past the cap is the same `TimeoutError: timed out` whichever cap it passed.
        at = [{"status": "TimeoutError: timed out"}, {"status": 200}]
        self.assertEqual(compare.statuses(at), "1 × TimeoutError: timed out, 1 × 200")


class Tables(unittest.TestCase):
    def results(self, linked_cap):
        meta = {
            "stamp": "t",
            "mode": "full",
            "docker_arch": "x86_64",
            "image": "i",
            "image_id": "sha256:0",
            "subjects": ["ours", "moalhaddar"],
            "torture_cpus": "2-3",
            "torture_memory": "12g",
        }
        if linked_cap is not None:
            meta.update(cap=compare.CAP, linked_cap_moalhaddar=linked_cap)
        return {"meta": meta, "torture": []}

    def test_the_tables_state_the_cap(self):
        self.assertIn("linked pictures 150 s a document", compare.render(self.results(150)))

    def test_a_run_recorded_before_the_option_renders_as_it_did(self):
        self.assertNotIn("linked pictures", compare.render(self.results(None)))

    def test_the_command_line_names_the_image_revision_beside_the_commit(self):
        results = self.results(None)
        results["meta"].update(commit="a46190b32130", image_revision="a46190b321307e15-dirty")
        self.assertIn(
            "at commit `a46190b32130`; the image labels revision `a46190b321307e15-dirty`;", compare.render(results)
        )

    def test_an_unlabelled_image_says_so_and_an_old_run_says_nothing(self):
        results = self.results(None)
        self.assertNotIn("revision", compare.render(results))
        results["meta"]["image_revision"] = None
        self.assertIn("the image carries no revision label", compare.render(results))


def soak_tsv(directory, mib, n=3000, sd=20, seed=7):
    """A soak.tsv of n requests whose LibreOffice RSS is mib(i) MiB plus a seeded normal spread of sd MiB."""
    rng = random.Random(seed)
    path = os.path.join(directory, "soak.tsv")
    with open(path, "w") as f:
        f.write("i\tdoc\tstatus\tseconds\tcurrent\tanon\trss_kib\tt\n")
        for i in range(n):
            rss = json.dumps({"tini": 1448, "instance": round((mib(i) + rng.gauss(0, sd)) * 1024)})
            f.write(f"{i}\treport.docx\t200\t1.0\t0\t0\t{rss}\t{i}\n")
    return path


class Soak(unittest.TestCase):
    """A soak grows when its memory still climbs over its second half; a climb early in the run that stops is a level,
    not growth."""

    def summary(self, mib, n=3000, sd=20):
        with tempfile.TemporaryDirectory() as d:
            return compare.soak_summary(soak_tsv(d, mib, n, sd))

    def test_a_climb_early_then_a_plateau_levels_off(self):
        # 316 MiB, a climb to 380 MiB between the third and the sixth tenth, then flat.
        s = self.summary(lambda i: 316 + 64 * min(max(i - 900, 0), 900) / 900)
        self.assertGreater(s["result"]["slope_per_1000"], 2 * s["result"]["slope_se"])
        self.assertEqual(s["result"]["growth"], "levels off")
        self.assertTrue(s["cell"].endswith("(1 % of the level): levels off"), s["cell"])

    def test_steady_growth_grows(self):
        s = self.summary(lambda i: 300 + 0.03 * i)
        self.assertEqual(s["result"]["growth"], "grows")
        self.assertAlmostEqual(
            s["result"]["slope_second_half_per_1000"], 30, delta=3 * s["result"]["slope_second_half_se"]
        )

    def test_flat_does_not_grow(self):
        s = self.summary(lambda i: 450)
        self.assertEqual(s["result"]["growth"], "does not grow")

    def test_growth_too_slow_for_the_second_half_to_resolve_still_grows(self):
        # moalhaddar/docx-to-pdf's soak on its reports: 1000 requests, +46 MiB per 1000 under a 65 MiB spread. The
        # second half alone cannot tell that from flat, but it is not below the nine tenths' climb either.
        s = self.summary(lambda i: 290 + 0.046 * i, n=1000, sd=65)
        self.assertLess(s["result"]["slope_second_half_per_1000"], 2 * s["result"]["slope_second_half_se"])
        self.assertEqual(s["result"]["growth"], "grows")

    def test_a_resolved_rise_under_the_floor_does_not_grow(self):
        # The release image's soak: 391 MiB, 1 MiB from one request to the next, +0.5 MiB per 1000. The fit resolves
        # the rise, but 1.5 MiB over 3000 conversions is not growth.
        s = self.summary(lambda i: 390 + 0.0005 * i, sd=1)
        self.assertGreater(s["result"]["slope_per_1000"], 2 * s["result"]["slope_se"])
        self.assertAlmostEqual(s["result"]["growth_floor_per_1000"], 3.9, delta=0.1)
        self.assertEqual(s["result"]["growth"], "does not grow")

    def test_a_rise_over_the_floor_grows(self):
        # 2 % of the level per 1000 at the same tight spread.
        s = self.summary(lambda i: 300 + 0.006 * i, sd=1)
        self.assertGreater(s["result"]["slope_per_1000"], s["result"]["growth_floor_per_1000"])
        self.assertEqual(s["result"]["growth"], "grows")

    def test_the_floor_is_one_percent_of_the_level_either_side(self):
        self.assertEqual(compare.growth(3.9, 0.1, 3.9, 0.2, floor=4.0), "does not grow")
        self.assertEqual(compare.growth(4.1, 0.1, 4.1, 0.2, floor=4.0), "grows")
        self.assertEqual(compare.growth_floor(400), 4.0)

    def test_the_cell_names_the_floor(self):
        s = self.summary(lambda i: 390 + 0.0005 * i, sd=1)
        self.assertRegex(
            s["cell"], r"over the second half; floor 3\.9 MiB per 1000 \(1 % of the level\): does not grow$"
        )

    def test_the_nine_tenths_slope_stays_for_older_results(self):
        s = self.summary(lambda i: 300 + 0.03 * i)
        self.assertIn("slope_per_1000", s["result"])
        self.assertIn("slope_se", s["result"])
        self.assertRegex(
            s["cell"], r"MiB per 1000 over the last nine tenths, [-+][0-9.]+ ± [0-9.]+ over the second half"
        )


class Host(unittest.TestCase):
    """The host comes from the bench's provenance.py, so compare and bench.py record and print it by one rule."""

    INFO = {
        "NCPU": 6,
        "MemTotal": 12 * 2**30,
        "OperatingSystem": "Ubuntu",
        "KernelVersion": "6.8",
        "Architecture": "aarch64",
        "ServerVersion": "28.4.0",
        "CgroupVersion": "2",
        "CgroupDriver": "cgroupfs",
    }

    def test_the_runner_reads_the_kernel_as_the_bench_does(self):
        seen = {"cpu_model": "Arm CPU", "hypervisor": "Apple Virtualization.framework", "memory_bytes": 1}
        with (
            mock.patch.object(compare.provenance, "linux_host", return_value=seen),
            mock.patch.object(bench, "sh", return_value=json.dumps(self.INFO)),
        ):
            h = compare.docker_host()
        self.assertEqual(h, compare.provenance.with_docker_info(seen, self.INFO))
        self.assertEqual((h["vcpus"], h["memory_bytes"], h["hypervisor"]), (6, 12 * 2**30, seen["hypervisor"]))

    def test_the_machine_comes_from_compare_sh(self):
        with mock.patch.dict(os.environ, {"COMPARE_MACHINE": json.dumps({"cpu_model": "Apple M1 Max"})}):
            self.assertEqual(compare.machine(), {"cpu_model": "Apple M1 Max"})
        for raw in ("null", "", "not json"):
            with mock.patch.dict(os.environ, {"COMPARE_MACHINE": raw}):
                self.assertIsNone(compare.machine())

    def test_the_line_names_the_hypervisor_and_the_chip(self):
        mach = {"cpu_model": "Apple M1 Max", "levels": [{"name": "Performance", "cores": 8, "caches": {}}]}
        host = {"vcpus": 6, "arch": "aarch64", "hypervisor": "Apple Virtualization.framework"}
        line = compare.host_line({"host": host, "machine": mach})
        self.assertEqual(line, compare.provenance.host_line(host, mach))
        self.assertIn("Apple M1 Max", line)
        self.assertIn("a VM under Apple Virtualization.framework", line)

    def test_a_run_recorded_before_says_only_whether_there_was_a_hypervisor(self):
        old = {"cpu_model": "Xeon", "logical_cpus": 8, "arch": "x86_64", "hypervisor": True}
        self.assertIn("a VM under a hypervisor", compare.host_line({"host": old}))
        self.assertNotIn("a VM", compare.host_line({"host": {**old, "hypervisor": False}}))
        self.assertEqual(compare.host_line({}), "not recorded by this run")


class Pinned(unittest.TestCase):
    def test_a_ref_given_by_digest_keeps_its_one_digest(self):
        ref = "quay.io/oxidestack/docx-to-pdf:1.0.0@sha256:" + "a" * 64
        self.assertEqual(compare.pinned(ref, "sha256:" + "a" * 64), ref)

    def test_a_built_image_is_pinned_to_its_id(self):
        self.assertEqual(compare.pinned("docx-to-pdf:compare", "sha256:1"), "docx-to-pdf:compare@sha256:1")


if __name__ == "__main__":
    unittest.main()
