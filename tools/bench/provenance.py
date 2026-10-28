#!/usr/bin/env python3
"""What a bench run records beside its results, so a figure says where it was measured and can be measured again:
the host, the command, the commits, the images by digest, the documents' checksums and seeds.

Standard library only. Two hosts matter. The Docker host runs the service: on a Linux machine it is the machine
itself, on a Mac a Linux VM, which hides the chip's name, its clocks and its caches. The
machine runs bench.py: on a Mac it names the chip; on Linux it is the Docker host. `docker_host()` reads the first
by running this file in the tool image (`python provenance.py` prints `linux_host()` as JSON); `machine()` reads the
second where bench.py runs. A guest never sees how its vCPUs map to the physical cores: the pinning recorded is the
run's own cpusets inside the VM.

No cloud's or provider's name is recorded: a hypervisor is named by its technology (KVM, Apple Virtualization).
"""

import hashlib
import json
import os
import platform
import shlex
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def read_file(path):
    try:
        with open(path) as f:
            return f.read().strip()
    except OSError:
        return None


def cpuinfo(text):
    """/proc/cpuinfo as one dict a logical CPU."""
    out = []
    for block in (text or "").split("\n\n"):
        fields = {k.strip(): v.strip() for k, _, v in (line.partition(":") for line in block.splitlines())}
        if any(fields):
            out.append(fields)
    return out


def cpu_list_length(text):
    """The number of CPUs in a list such as `0-3,8`."""
    n = 0
    for part in (text or "").split(","):
        lo, _, hi = part.strip().partition("-")
        if lo.isdigit():
            n += int(hi or lo) - int(lo) + 1
    return n or None


# DMI product or vendor fragment → the hypervisor's technology. Never the provider: a cloud's name stays out.
HYPERVISORS = [
    ("apple virtualization", "Apple Virtualization.framework"),
    ("kvm", "KVM"),
    ("qemu", "QEMU/KVM"),
    ("openstack", "KVM"),
    ("vmware", "VMware"),
    ("virtualbox", "VirtualBox"),
    ("hvm domu", "Xen"),
    ("xen", "Xen"),
    ("virtual machine", "Hyper-V"),
]


def hypervisor(flags, product, vendor):
    """The hypervisor's technology when this kernel runs in a VM, else None. x86 CPUs report a `hypervisor` flag;
    Arm guests do not, so the DMI product and vendor decide."""
    seen = f"{product or ''} {vendor or ''}".lower()
    for fragment, name in HYPERVISORS:
        if fragment in seen:
            return name
    return "a hypervisor" if "hypervisor" in (flags or "").split() else None


def linux_host(read=read_file):
    """This Linux kernel's CPU, topology, clocks, caches and memory, from /proc and /sys (`read` is the file reader,
    replaced in the tests)."""
    cpus = cpuinfo(read("/proc/cpuinfo"))
    first = cpus[0] if cpus else {}
    model = first.get("model name")
    if not model and "CPU implementer" in first:
        # Arm's CPUs name only an implementer and a part; under a Mac's VM the chip's name is not visible.
        vendor = {"0x41": "Arm", "0x61": "Apple"}.get(first["CPU implementer"], first["CPU implementer"])
        model = f"{vendor} CPU, part {first.get('CPU part')} (the chip's model name is not visible here)"
    caches = {}
    base = "/sys/devices/system/cpu/cpu0/cache"
    for i in range(8):
        level, kind, size = (read(f"{base}/index{i}/{n}") for n in ("level", "type", "size"))
        if level and size:
            caches[f"L{level}{ {'Data': 'd', 'Instruction': 'i'}.get(kind, '') }"] = size
    khz = {
        k: read(f"/sys/devices/system/cpu/cpu0/cpufreq/{f}") or ""
        for k, f in (
            ("max", "cpuinfo_max_freq"),
            ("base", "base_frequency"),
        )
    }
    memory = next(
        (
            int(line.split()[1]) * 1024
            for line in (read("/proc/meminfo") or "").splitlines()
            if line.startswith("MemTotal:")
        ),
        None,
    )
    return {
        "cpu_model": model,
        "logical_cpus": len(cpus) or None,
        "online": read("/sys/devices/system/cpu/online"),
        "sockets": len({c["physical id"] for c in cpus if "physical id" in c}) or None,
        "cores_per_socket": int(first["cpu cores"]) if first.get("cpu cores", "").isdigit() else None,
        "threads_per_core": cpu_list_length(read("/sys/devices/system/cpu/cpu0/topology/thread_siblings_list")),
        "mhz_now": float(first["cpu MHz"]) if first.get("cpu MHz") else None,
        "mhz_base": int(khz["base"]) // 1000 if khz["base"].isdigit() else None,
        "mhz_max": int(khz["max"]) // 1000 if khz["max"].isdigit() else None,
        "caches": caches,
        "memory_bytes": memory,
        "hypervisor": hypervisor(
            first.get("flags"), read("/sys/class/dmi/id/product_name"), read("/sys/class/dmi/id/sys_vendor")
        ),
        "kernel": read("/proc/sys/kernel/osrelease"),
    }


def run(*args):
    """A command's stdout, or None when it is missing or fails."""
    try:
        done = subprocess.run(args, capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return done.stdout.strip() if done.returncode == 0 else None


def darwin_machine(sysctl):
    """A Mac's chip from `sysctl` values (`sysctl` maps a name to its value or None): the model, the performance
    levels with their cores and caches, the memory. macOS does not expose an Apple chip's clocks."""

    def number(name):
        value = sysctl(name)
        return int(value) if value and value.isdigit() else None

    levels = []
    for i in range(number("hw.nperflevels") or 0):
        p = f"hw.perflevel{i}"
        caches = {
            label: number(f"{p}.{key}")
            for label, key in (("L1d", "l1dcachesize"), ("L1i", "l1icachesize"), ("L2", "l2cachesize"))
            if number(f"{p}.{key}")
        }
        levels.append(
            {
                "name": sysctl(f"{p}.name"),
                "cores": number(f"{p}.physicalcpu"),
                "caches": {k: f"{v // 2**20}M" if v % 2**20 == 0 else f"{v // 1024}K" for k, v in caches.items()},
                "l2_shared_by": number(f"{p}.cpusperl2"),
            }
        )
    hz = number("hw.cpufrequency")
    return {
        "cpu_model": sysctl("machdep.cpu.brand_string"),
        "physical_cores": number("hw.physicalcpu"),
        "logical_cpus": number("hw.logicalcpu"),
        "levels": levels,
        "mhz_max": hz // 1_000_000 if hz else None,
        "clock_note": None if hz else "macOS does not expose this chip's clocks",
        "memory_bytes": number("hw.memsize"),
        "os": f"macOS {sysctl('kern.osproductversion')}",
        "kernel": f"Darwin {sysctl('kern.osrelease')}",
    }


def docker_vm(info_json):
    """The Docker VM's vCPUs, memory, architecture and OS from `docker info`'s JSON, or None when Docker answered
    nothing usable."""
    # Provenance describes a run; an unreadable answer from Docker leaves the field out rather than stopping the run.
    try:
        info = json.loads(info_json or "null")
    except ValueError:
        return None
    if not isinstance(info, dict):
        return None
    return {
        "vcpus": info.get("NCPU"),
        "memory_bytes": info.get("MemTotal"),
        "arch": info.get("Architecture"),
        "os": info.get("OperatingSystem"),
    }


def machine():
    """The machine bench.py runs on: a Mac's chip, or a Linux machine's own /proc and /sys."""
    system = platform.system()
    if system == "Darwin":
        out = darwin_machine(lambda name: run("sysctl", "-n", name))
        # The VM is read through the Docker engine it runs, so any VM manager is recorded the same way.
        vm = docker_vm(run("docker", "info", "--format", "{{json .}}"))
        if vm:
            out["docker_vm"] = vm
        return out
    if system == "Linux":
        out = linux_host()
        release = dict(line.split("=", 1) for line in (read_file("/etc/os-release") or "").splitlines() if "=" in line)
        out["os"] = release.get("PRETTY_NAME", "Linux").strip('"')
        return out
    return {"cpu_model": platform.processor() or None, "os": f"{system} {platform.release()}"}


def docker_host(sh, tools_image, bench_dir, name):
    """The Docker host's kernel as a container sees it, with `docker info`'s OS, engine and cgroup. `sh` is bench.sh."""
    info = json.loads(sh("docker", "info", "--format", "{{json .}}", quiet=True) or "{}")
    seen = sh(
        "docker",
        "run",
        "--rm",
        "--name",
        name,
        "-v",
        f"{bench_dir}:/bench:ro",
        tools_image,
        "python",
        "/bench/provenance.py",
        quiet=True,
    )
    return with_docker_info(json.loads(seen), info)


def with_docker_info(seen, info):
    """A kernel's view (`linux_host()`, read in a container on the Docker host) completed by `docker info`'s
    vCPUs, memory, OS, kernel, architecture, engine and cgroup."""
    out = dict(seen)
    out.update(
        {
            "vcpus": info.get("NCPU"),
            "memory_bytes": info.get("MemTotal") or out.get("memory_bytes"),
            "os": info.get("OperatingSystem"),
            "kernel": info.get("KernelVersion") or out.get("kernel"),
            "arch": info.get("Architecture"),
            "docker": info.get("ServerVersion"),
            "cgroup": info.get("CgroupVersion"),
            "cgroup_driver": info.get("CgroupDriver"),
        }
    )
    return out


def commit(root=ROOT):
    """The checkout's commit, and whether tracked files differ from it."""
    head = run("git", "-C", root, "rev-parse", "HEAD")
    status = run("git", "-C", root, "status", "--porcelain", "--untracked-files=no")
    return {"commit": head, "dirty": bool(status) if status is not None else None}


def image(sh, ref):
    """An image by its id, its repository digests and the commit it labels (`org.opencontainers.image.revision`)."""
    if not ref:
        return None
    raw = sh("docker", "image", "inspect", "--format", "{{json .}}", ref, check=False, quiet=True)
    if not raw:
        return {"ref": ref, "id": None}
    data = json.loads(raw)
    labels = (data.get("Config") or {}).get("Labels") or {}
    return {
        "ref": ref,
        "id": data.get("Id"),
        "digests": data.get("RepoDigests") or [],
        "revision": labels.get("org.opencontainers.image.revision"),
        "arch": data.get("Architecture"),
    }


def document_seed(name):
    """The seed generate.py's `rng` draws a document from: the first 8 bytes of SHA-256 of its name."""
    return int.from_bytes(hashlib.sha256(name.encode()).digest()[:8], "little")


def documents(rows):
    """Each manifest row's checksum, size and seed (the copied fixtures have none: their bytes are the source)."""
    return {
        r["name"]: {
            "sha256": r.get("sha256"),
            "bytes": int(r["bytes"]) if str(r.get("bytes", "")).isdigit() else None,
            "seed": None if r.get("class") in ("objects", "hostile") else document_seed(r["name"]),
        }
        for r in rows
    }


def command(argv=None):
    """The command line, its script relative to the checkout."""
    argv = sys.argv if argv is None else argv
    return shlex.join([os.path.relpath(os.path.abspath(argv[0]), ROOT), *argv[1:]])


def gib(n):
    return f"{n / 2**30:.1f} GiB" if n else None


def join(*parts):
    return "; ".join(p for p in parts if p)


def host_line(docker, mach=None):
    """One line for a report: the machine's chip (where bench.py ran), then the Docker host the service ran on."""
    if not docker and not mach:
        return "not recorded by this run"

    def level(lv):
        caches = [
            f"{k} {v}" + (f" shared by {lv['l2_shared_by']} cores" if k == "L2" and lv.get("l2_shared_by") else "")
            for k, v in (lv.get("caches") or {}).items()
        ]
        return f"{lv['cores']} {str(lv['name']).lower()} cores" + (f" ({', '.join(caches)})" if caches else "")

    parts = []
    if mach and mach.get("levels"):
        levels = ", ".join(level(lv) for lv in mach["levels"])
        parts.append(
            join(
                f"machine: {mach.get('cpu_model')}, {levels}",
                f"{mach['mhz_max']} MHz max" if mach.get("mhz_max") else mach.get("clock_note"),
                gib(mach.get("memory_bytes")),
                mach.get("os"),
            )
        )
    if docker:
        caches = " ".join(f"{k} {v}" for k, v in (docker.get("caches") or {}).items())
        topology = join(
            f"{docker.get('vcpus') or docker.get('logical_cpus')} vCPU",
            f"{docker['sockets']} socket(s) × {docker['cores_per_socket']} cores"
            if docker.get("cores_per_socket")
            else "",
            f"{docker['threads_per_core']} thread(s) a core" if docker.get("threads_per_core") else "",
        )
        clocks = join(
            f"{docker['mhz_base']} MHz base" if docker.get("mhz_base") else "",
            f"{docker['mhz_max']} MHz max" if docker.get("mhz_max") else "",
            f"{docker['mhz_now']:.0f} MHz reported" if docker.get("mhz_now") else "",
        )
        parts.append(
            join(
                f"Docker host: {docker.get('cpu_model') or 'CPU model not reported'} ({docker.get('arch')})",
                topology,
                clocks,
                f"caches a core: {caches}" if caches else "",
                gib(docker.get("memory_bytes")),
                f"{docker.get('os')}, kernel {docker.get('kernel')}, Docker {docker.get('docker')}, "
                f"cgroup v{docker.get('cgroup')}",
                f"a VM under {docker['hypervisor']} (vCPU pinning to the host's cores is not visible from the guest)"
                if docker.get("hypervisor")
                else "",
            )
        )
    return " | ".join(parts)


if __name__ == "__main__":
    # `python provenance.py` prints this kernel's view (docker_host runs it in a container); `machine` the machine's.
    print(json.dumps(machine() if sys.argv[1:] == ["machine"] else linux_host()))
