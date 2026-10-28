"""The bench's server-side sampler: one JSON line per tick.

    python sampler.py --cgroup DIR --metrics URL --out FILE [--interval 1] [--scrape-every 5]

Run as a sidecar of the service container: `--pid container:<service>` (its processes' /proc), the host's
/sys/fs/cgroup mounted read-only (DIR is the service's cgroup in it), and the service's network. Each tick
records the service cgroup's cumulative counters and gauges (cpu.stat, memory.current/.peak/.stat/.events,
io.stat, the cpu/memory/io pressure files), the VM's pressure (/proc/pressure, not namespaced), and every
process's comm, RSS, VmHWM and CPU ticks; every --scrape-every seconds, the service's own metrics. Cumulative
values are written as read: the analysis takes differences over its window. Stops on SIGTERM after a last tick.
"""

import argparse
import json
import os
import re
import signal
import sys
import time
import urllib.request

MEMORY_STAT = ("anon", "file", "shmem", "file_mapped", "file_dirty", "file_writeback", "kernel", "sock")
METRIC_PREFIXES = ("docx_to_pdf_", "http_server_", "process_")


def read(path):
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        return None


def flat(text):
    """`key value` lines as a dict of numbers."""
    out = {}
    for line in (text or "").splitlines():
        parts = line.split()
        if len(parts) == 2:
            try:
                out[parts[0]] = int(parts[1])
            except ValueError:
                pass
    return out


def pressure(text):
    """`some avg10=.. avg60=.. avg300=.. total=..` lines as {"some": {...}, "full": {...}}."""
    out = {}
    for line in (text or "").splitlines():
        kind, *fields = line.split()
        out[kind] = {k: float(v) for k, v in (f.split("=") for f in fields)}
    return out


def io_stat(text):
    """`MAJ:MIN rbytes=.. wbytes=.. rios=.. wios=..` lines as {"MAJ:MIN": {...}}."""
    out = {}
    for line in (text or "").splitlines():
        dev, *fields = line.split()
        out[dev] = {k: int(v) for k, v in (f.split("=") for f in fields)}
    return out


def cgroup(root):
    def single(name):
        t = read(f"{root}/{name}")
        return int(t) if t and t.strip().isdigit() else None

    stat = flat(read(f"{root}/memory.stat"))
    return {
        "cpu": flat(read(f"{root}/cpu.stat")),
        "memory_current": single("memory.current"),
        "memory_peak": single("memory.peak"),
        "memory_max": (read(f"{root}/memory.max") or "").strip(),
        "memory_stat": {k: stat.get(k) for k in MEMORY_STAT},
        "memory_events": flat(read(f"{root}/memory.events")),
        "io": io_stat(read(f"{root}/io.stat")),
        "pressure": {r: pressure(read(f"{root}/{r}.pressure")) for r in ("cpu", "memory", "io")},
    }


def processes(me):
    out = []
    for entry in os.listdir("/proc"):
        if not entry.isdigit() or int(entry) == me:
            continue
        status = read(f"/proc/{entry}/status")
        stat = read(f"/proc/{entry}/stat")
        if not status or not stat:
            continue
        fields = dict(line.split(":", 1) for line in status.splitlines() if ":" in line)

        def kib(k, fields=fields):
            return int(fields[k].split()[0]) * 1024 if k in fields else None

        # Fields after the parenthesised comm: utime and stime are the 14th and 15th of the whole line.
        rest = stat.rsplit(")", 1)[1].split()
        out.append(
            {
                "pid": int(entry),
                "ppid": int(fields.get("PPid", "0").strip() or 0),
                "comm": fields.get("Name", "").strip(),
                "rss": kib("VmRSS"),
                "hwm": kib("VmHWM"),
                "anon": kib("RssAnon"),
                "file": kib("RssFile"),
                "shmem": kib("RssShmem"),
                "cpu_ticks": int(rest[11]) + int(rest[12]),
            }
        )
    return out


def scrape(url):
    try:
        with urllib.request.urlopen(url, timeout=2) as response:
            text = response.read().decode()
    except OSError as error:
        return {"error": str(error)}
    out = {}
    for line in text.splitlines():
        if line.startswith(METRIC_PREFIXES):
            series, _, value = line.rpartition(" ")
            series = re.sub(r'otel_scope_name="[^"]*",?', "", series).replace(",}", "}").replace("{}", "")
            try:
                out[series] = float(value)
            except ValueError:
                pass
    return out


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--cgroup", required=True)
    parser.add_argument("--metrics", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--interval", type=float, default=1.0)
    parser.add_argument("--scrape-every", type=float, default=5.0)
    args = parser.parse_args()
    if not os.path.isdir(args.cgroup):
        print(f"no cgroup at {args.cgroup}", file=sys.stderr)
        return 2

    stopping = []
    signal.signal(signal.SIGTERM, lambda *_: stopping.append(True))
    me = os.getpid()
    ticks_per_second = os.sysconf("SC_CLK_TCK")
    last_scrape = 0.0
    with open(args.out, "a") as out:
        out.write(json.dumps({"t": time.time(), "clk_tck": ticks_per_second}) + "\n")
        while True:
            now = time.time()
            sample = {
                "t": now,
                "cg": cgroup(args.cgroup),
                "vm_pressure": {r: pressure(read(f"/proc/pressure/{r}")) for r in ("cpu", "memory", "io")},
                "procs": processes(me),
            }
            if now - last_scrape >= args.scrape_every or stopping:
                sample["metrics"] = scrape(args.metrics)
                last_scrape = now
            out.write(json.dumps(sample, separators=(",", ":")) + "\n")
            out.flush()
            if stopping:
                return 0
            time.sleep(max(0.0, args.interval - (time.time() - now)))
            if stopping:
                continue


if __name__ == "__main__":
    sys.exit(main())
