"""The bench's figures (mean ± σ (n), the 2σ test, cells of replicates) and what a run records about its host."""

import json
import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "bench"))

import figures  # noqa: E402
import provenance  # noqa: E402


def summary(throughput, p50=None, peak=None):
    return {"throughput": throughput, "latency": {"p50": p50}, "memory_peak": peak}


class Figures(unittest.TestCase):
    def test_mean_sigma_n(self):
        mean, sd, n = figures.stats([1.0, 2.0, 3.0])
        self.assertEqual((mean, n), (2.0, 3))
        self.assertAlmostEqual(sd, 1.0, msg="the sample standard deviation, n - 1")
        self.assertEqual(figures.record((2.0, 1.0, 3), 2), "2.00 ± 1.00 (3)")

    def test_one_replicate_has_no_sigma(self):
        self.assertEqual(figures.stats([4.0]), (4.0, None, 1))
        self.assertEqual(figures.spread([4.0], 1), "4.0 (1 run, no σ)")
        self.assertEqual(figures.spread([None]), "—")

    def test_sigma_keeps_one_significant_digit(self):
        self.assertEqual(figures.record((29.9, 0.0012, 2), 1), "29.9 ± 0.001 (2)")
        self.assertEqual(figures.record((1219.0, 12.4, 3), 0), "1 219 ± 12 (3)")

    def test_two_sigma(self):
        delta, bound = figures.agreement((10.0, 1.0, 4), (9.0, 1.0, 4))
        self.assertEqual(delta, 1.0)
        self.assertAlmostEqual(bound, 2 * (0.25 + 0.25) ** 0.5)
        self.assertEqual(figures.agreement((10.0, None, 1), (9.0, 1.0, 4)), (None, None))

    def test_replicates_form_a_cell(self):
        runs = [
            ("/data/runs/usl/p2/r1", summary(1.0, peak=2**20)),
            ("/data/runs/usl/p2/r2", summary(1.2, peak=3 * 2**20)),
            ("/data/runs/adhoc/smoke-20261028T120000", summary(0.5)),
        ]
        table = figures.cells(runs)
        self.assertEqual(list(table), ["usl/p2", "adhoc/smoke-20261028T120000"])
        p2 = table["usl/p2"]["figures"]
        self.assertAlmostEqual(p2["throughput"][0], 1.1)
        self.assertEqual(p2["throughput"][2], 2)
        self.assertEqual(p2["peak"][0], 2.0, "memory in MiB")
        self.assertIsNone(p2["p50"], "a figure no replicate recorded")
        lines = figures.cells_table(table)
        self.assertIn("| usl/p2 | 2 | 1.100 ± 0.141 (2) |", lines[2])
        self.assertIn("0.500 (1 run, no σ)", lines[3])

    def test_against(self):
        new = figures.cells([(f"/r/m/c/r{i}", summary(v)) for i, v in enumerate((1.0, 1.1, 1.2), 1)])
        same = figures.cells([(f"/o/m/c/r{i}", summary(v)) for i, v in enumerate((1.05, 1.1, 1.15), 1)])
        far = figures.cells([(f"/o/m/c/r{i}", summary(v)) for i, v in enumerate((2.0, 2.1, 2.2), 1)])
        _, count = figures.against_table(new, same)
        self.assertEqual(count, {"agrees": 1, "differs": 0, "no σ": 0})
        rows, count = figures.against_table(new, far)
        self.assertEqual(count["differs"], 1)
        self.assertIn("differs: Δ -47.6 %", rows[2])


CPUINFO_X86 = """processor\t: 0
model name\t: Intel(R) Xeon(R) Gold 6248R CPU @ 3.00GHz
cpu MHz\t\t: 2999.998
physical id\t: 0
cpu cores\t: 4
flags\t\t: fpu sse2 hypervisor avx2

processor\t: 1
model name\t: Intel(R) Xeon(R) Gold 6248R CPU @ 3.00GHz
cpu MHz\t\t: 2999.998
physical id\t: 0
cpu cores\t: 4
flags\t\t: fpu sse2 hypervisor avx2
"""


class Host(unittest.TestCase):
    def test_linux_host_reads_cpu_caches_memory(self):
        files = {
            "/proc/cpuinfo": CPUINFO_X86,
            "/proc/meminfo": "MemTotal:       16384000 kB\nMemFree: 1 kB",
            "/sys/devices/system/cpu/cpu0/topology/thread_siblings_list": "0",
            "/sys/devices/system/cpu/cpu0/cache/index0/level": "1",
            "/sys/devices/system/cpu/cpu0/cache/index0/type": "Data",
            "/sys/devices/system/cpu/cpu0/cache/index0/size": "32K",
            "/sys/devices/system/cpu/cpu0/cache/index2/level": "2",
            "/sys/devices/system/cpu/cpu0/cache/index2/type": "Unified",
            "/sys/devices/system/cpu/cpu0/cache/index2/size": "4096K",
            "/sys/class/dmi/id/product_name": "OpenStack Nova",
            "/proc/sys/kernel/osrelease": "6.8.0-64-generic",
        }
        h = provenance.linux_host(files.get)
        self.assertEqual(h["cpu_model"], "Intel(R) Xeon(R) Gold 6248R CPU @ 3.00GHz")
        self.assertEqual((h["logical_cpus"], h["sockets"], h["cores_per_socket"], h["threads_per_core"]), (2, 1, 4, 1))
        self.assertEqual(h["caches"], {"L1d": "32K", "L2": "4096K"})
        self.assertEqual(h["memory_bytes"], 16384000 * 1024)
        self.assertEqual(h["hypervisor"], "KVM", "the technology, never the provider's product name")
        self.assertNotIn("OpenStack", provenance.host_line(h))

    def test_arm_guest_names_no_chip(self):
        h = provenance.linux_host(
            {
                "/proc/cpuinfo": "processor\t: 0\nCPU implementer\t: 0x61\nCPU part\t: 0x000\n",
                "/sys/class/dmi/id/product_name": "Apple Virtualization Generic Platform",
            }.get
        )
        self.assertIn("Apple CPU", h["cpu_model"])
        self.assertIn("not visible", h["cpu_model"])
        self.assertEqual(h["hypervisor"], "Apple Virtualization.framework")
        self.assertIsNone(provenance.hypervisor("fpu sse2", None, None), "bare metal")

    def test_darwin_machine_names_the_chip(self):
        values = {
            "machdep.cpu.brand_string": "Apple M1 Max",
            "hw.physicalcpu": "10",
            "hw.logicalcpu": "10",
            "hw.memsize": str(64 * 2**30),
            "hw.nperflevels": "2",
            "hw.perflevel0.name": "Performance",
            "hw.perflevel0.physicalcpu": "8",
            "hw.perflevel0.l2cachesize": str(12 * 2**20),
            "hw.perflevel0.l1dcachesize": "131072",
            "hw.perflevel1.name": "Efficiency",
            "hw.perflevel1.physicalcpu": "2",
            "kern.osproductversion": "27.0.1",
            "kern.osrelease": "27.0.0",
        }
        m = provenance.darwin_machine(values.get)
        self.assertEqual(m["cpu_model"], "Apple M1 Max")
        self.assertEqual([(lv["name"], lv["cores"]) for lv in m["levels"]], [("Performance", 8), ("Efficiency", 2)])
        self.assertEqual(m["levels"][0]["caches"], {"L1d": "128K", "L2": "12M"})
        self.assertIsNone(m["mhz_max"])
        line = provenance.host_line({"vcpus": 6, "arch": "aarch64", "hypervisor": "Apple Virtualization.framework"}, m)
        self.assertIn("machine: Apple M1 Max, 8 performance cores", line)
        self.assertIn("Docker host:", line)
        self.assertIn("6 vCPU", line)

    def test_docker_vm_reads_docker_info(self):
        info = json.dumps(
            {"NCPU": 6, "MemTotal": 12515700736, "Architecture": "aarch64", "OperatingSystem": "Ubuntu 24.04.2 LTS"}
        )
        self.assertEqual(
            provenance.docker_vm(info),
            {"vcpus": 6, "memory_bytes": 12515700736, "arch": "aarch64", "os": "Ubuntu 24.04.2 LTS"},
        )

    def test_docker_vm_leaves_a_missing_field_empty(self):
        self.assertEqual(
            provenance.docker_vm('{"NCPU": 4}'), {"vcpus": 4, "memory_bytes": None, "arch": None, "os": None}
        )

    def test_docker_vm_is_none_when_docker_answers_nothing_usable(self):
        # run() returns None when docker is absent or fails; an unreadable answer must not stop the run either.
        for answer in (None, "", "not json", "{", "null", "[1, 2]"):
            with self.subTest(answer=answer):
                self.assertIsNone(provenance.docker_vm(answer))

    def test_documents_carry_checksum_and_seed(self):
        rows = [
            {"name": "text-10p", "class": "text", "bytes": "1234", "sha256": "ab"},
            {"name": "objects-fill", "class": "objects", "bytes": "9", "sha256": "cd"},
        ]
        docs = provenance.documents(rows)
        self.assertEqual(docs["text-10p"]["sha256"], "ab")
        self.assertEqual(docs["text-10p"]["seed"], provenance.document_seed("text-10p"))
        self.assertIsNone(docs["objects-fill"]["seed"], "a copied fixture has no seed")

    def test_command_is_relative_to_the_checkout(self):
        argv = [os.path.join(provenance.ROOT, "tools", "bench", "bench.py"), "cell", "x", "--docs", "a b"]
        self.assertEqual(provenance.command(argv), "tools/bench/bench.py cell x --docs 'a b'")


if __name__ == "__main__":
    unittest.main()
