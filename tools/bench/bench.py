#!/usr/bin/env python3
"""The bench (method: tools/bench/README.md): runs the service under load and records what it costs.

    tools/bench/bench.py tools                        build the tool image (generator, sampler, analysis)
    tools/bench/bench.py generate [--hostile] [--heavy] [--only NAME,...]
                                                      the documents, into .bench/docs/ (manifest.tsv)
    tools/bench/bench.py cell NAME [options]          one run: fresh containers, warm-up, steady window, cleanup
    tools/bench/bench.py matrix FILE [--reps N] [--seed S] [--only CELL,...]
                                                      every cell of FILE × N replicates, in random order; resumes
    tools/bench/bench.py report [RUNS_DIR ...] [--out DIR] [--against DIR]
                                                      summaries (summary.json per run, runs.csv), each cell's
                                                      figures as mean ± σ (n) (cells.md), the graphs; --against
                                                      judges each figure against another report's at 2σ
    tools/bench/bench.py preflight                    what else loads the VM now
    tools/bench/bench.py probe [--cpus C]             this host's s1, contention and disk MB/s, for size --platform
    tools/bench/bench.py probe --url URL              the same from a running service's probe endpoint
    tools/bench/bench.py size hardware --cpus C --memory GIB [--disk-mbps M | --tmpfs] [--platform FILE | --s1 S]
                                                      the launch settings for that hardware and the /tmp they
                                                      need
    tools/bench/bench.py size target --latency S [--quantile Q] --rate R[/s|/min|/h]
                                                      the pods, their shape and settings for that load
    tools/bench/bench.py size hardware|target --typical ...
                                                      memory and /tmp from the typical mix's fit, not the worst
                                                      case
    tools/bench/bench.py size explain [--cpus C] [--platform FILE]
                                                      the platform's numbers and every formula, worked out for C

Host side: Python 3 standard library and the docker CLI. Everything else runs in containers named
docx-to-pdf-bench-*, all removed when a run ends, also on Ctrl-C. Results go to .bench/runs/ (git-ignored); the
docs and the runs must live under $HOME, which a macOS Docker VM shares with its host.

The service runs under the chart's security context (as image/test.sh), on cpus 0..C-1 of the VM; k6 and the
sampler share the last cpu, the Collector (tracing on) the one before it, so neither takes CPU from the service.
"""

import argparse
import csv
import datetime
import json
import os
import random
import shlex
import signal
import subprocess
import sys
import time
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BENCH = os.path.join(ROOT, "tools", "bench")
DATA = os.path.join(ROOT, ".bench")
DOCS = os.path.join(DATA, "docs")
RUNS = os.path.join(DATA, "runs")
# A local directory of LibreOffice's DOCX regression documents (the `objects` and `hostile` sets read it).
FIXTURES = os.environ.get("DOCX_TO_PDF_FIXTURES", "")

TOOLS_IMAGE = "docx-to-pdf-bench-tools:dev"
K6_IMAGE = "grafana/k6:2.3.0@sha256:9c2dee7f8ed74d317e4027c06a10f169b625638189de8d4555d0b3486a5aeb34"
COLLECTOR_IMAGE = (
    "otel/opentelemetry-collector-contrib:0.162.0"
    "@sha256:39923a8e431bd1f57be82411999d389fcfe40857492e4365456d97a4c1f74be6"
)
PREFIX = "docx-to-pdf-bench"

# What a run starts. oxidestack/docx-to-pdf: its image under the chart's security context. moalhaddar/docx-to-pdf:
# upstream's image (oracle.env) as an installation runs it: its own user, the multipart limits and Tomcat's timeout
# raised as the deployments raise them, no /metrics (the sampler records the failed scrape and the analysis skips it).
# gotenberg/gotenberg, for comparison: one LibreOffice per container (pool 1 only), its own user, its form route and
# field, its request timeout raised past any conversion; its other defaults as shipped, LibreOffice restarted every 10
# conversions among them.
SUBJECTS = {
    "ours": {"image": "docx-to-pdf:bench", "ready": "/readyz", "secure": True},
    "oracle": {"ready": "/actuator/health/readiness", "secure": False},
    "gotenberg": {
        "image": "gotenberg/gotenberg:8.38.1@sha256:aa552cc115db488e0f0bcb07446aac13071ad51505b642c84bb894500907b42b",
        "ready": "/health",
        "secure": False,
        "path": "/forms/libreoffice/convert",
        "field": "files",
    },
}


def oracle_image():
    with open(os.path.join(ROOT, "oracle.env")) as f:
        return next(line.split("=", 1)[1].strip() for line in f if line.startswith("ORACLE_IMAGE="))


def cgroup_dir(container_id):
    """The container's cgroup under the host's /sys/fs/cgroup: Docker's cgroupfs driver (a macOS Docker VM) or
    systemd's."""
    driver = sh("docker", "info", "--format", "{{.CgroupDriver}}", quiet=True)
    return f"system.slice/docker-{container_id}.scope" if driver == "systemd" else f"docker/{container_id}"


def sh(*args, check=True, capture=True, quiet=False):
    if not quiet:
        print("+", " ".join(shlex.quote(a) for a in args), file=sys.stderr, flush=True)
    result = subprocess.run(args, text=True, capture_output=capture)
    if check and result.returncode != 0:
        raise RuntimeError(
            f"{args[0]} {args[1] if len(args) > 1 else ''} failed ({result.returncode}): "
            f"{(result.stderr or '').strip()[-2000:]}"
        )
    return result.stdout.strip() if capture else ""


def vm_cpus():
    return int(sh("docker", "info", "--format", "{{.NCPU}}", quiet=True))


def manifest():
    path = os.path.join(DOCS, "manifest.tsv")
    if not os.path.exists(path):
        sys.exit("no .bench/docs/manifest.tsv: run `bench.py generate` first")
    with open(path) as f:
        return {row["name"]: row for row in csv.DictReader(f, delimiter="\t")}


def select_docs(spec):
    """Names or classes, comma-separated: `text-10p`, `mix-20mb,mix-50mb`, `class:objects`."""
    rows = manifest()
    chosen = []
    for item in spec.split(","):
        if item.startswith("class:"):
            chosen += [r for r in rows.values() if r["class"] == item[6:]]
        elif item in rows:
            chosen.append(rows[item])
        else:
            sys.exit(f"no document {item} in the manifest")
    return chosen


def preflight():
    """The VM's load before a run: other containers, and its pressure, so a run starts on a quiet host."""
    others = sh("docker", "ps", "--format", "{{.Names}}", quiet=True).splitlines()
    psi = sh(
        "docker",
        "run",
        "--rm",
        "--name",
        f"{PREFIX}-psi-{os.getpid()}",
        TOOLS_IMAGE,
        "python",
        "-c",
        "import json;print(json.dumps({r:open('/proc/pressure/'+r).read() for r in ('cpu','memory','io')}))",
        quiet=True,
    )
    # Device-mapper devices (LVM) stack on a disk, and io.stat charges a write to both (0049: dm-0 and sda, the same
    # bytes twice); the analysis leaves these out.
    stacked = sh(
        "docker",
        "run",
        "--rm",
        "--name",
        f"{PREFIX}-dm-{os.getpid()}",
        TOOLS_IMAGE,
        "python",
        "-c",
        "import glob;print(' '.join(open(p).read().strip() for p in glob.glob('/sys/block/dm-*/dev')))",
        quiet=True,
    ).split()
    stats = sh("docker", "stats", "--no-stream", "--format", "{{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}", quiet=True)
    # The VM's vCPUs are host threads: a busy host slows the run without the VM seeing it (0069: another session's
    # cargo build at load 10-13 on 10 cores made conversions 2-15× slower).
    return {
        "containers": [c for c in others if not c.startswith(PREFIX)],
        "vm_pressure": json.loads(psi),
        "docker_stats": stats.splitlines(),
        "host_loadavg": os.getloadavg(),
        "host_cpus": os.cpu_count(),
        "stacked_devices": stacked,
    }


def provenance_of(args, docs, n, cpus):
    """What makes the run's figures reproducible and says where they were measured (provenance.py): the Docker host
    and the machine, the cpusets, the command, the commit, the images by digest, the documents' checksums and seeds,
    and which replicate of how many. A failure to read any of it is recorded, never fatal."""
    import provenance

    out = {
        "command": provenance.command(),
        "cell_command": getattr(args, "cell_command", None),
        "matrix": getattr(args, "matrix", None),
        "matrix_seed": getattr(args, "matrix_seed", None),
        "replicate": getattr(args, "replicate", 1),
        "replicates": getattr(args, "replicates", 1),
        "pinning": {
            "service": f"0-{cpus - 1}" if cpus > 1 else "0",
            "k6_and_sampler": str(n - 1),
            "collector": str(n - 2) if args.tracing == "on" else None,
            "to_host_cores": "not visible from the guest",
        },
    }
    readers = {
        "host": lambda: provenance.docker_host(sh, TOOLS_IMAGE, BENCH, f"{PREFIX}-host-{os.getpid()}"),
        "machine": provenance.machine,
        "commit": provenance.commit,
        "images": lambda: {
            "service": provenance.image(sh, args.image),
            "k6": provenance.image(sh, K6_IMAGE),
            "collector": provenance.image(sh, COLLECTOR_IMAGE) if args.tracing == "on" else None,
            "tools": provenance.image(sh, TOOLS_IMAGE),
        },
        "documents": lambda: provenance.documents(docs),
    }
    for key, reader in readers.items():
        try:
            out[key] = reader()
        except (OSError, RuntimeError, ValueError, KeyError) as error:
            out[key] = {"error": str(error)}
    return out


def cpu_some_avg10(pre):
    first = pre["vm_pressure"]["cpu"].splitlines()[0]
    return float(first.split()[1].split("=")[1])


class Run:
    """One cell's containers, network and volume; `close` removes every one of them."""

    def __init__(self, run_id):
        self.id = run_id
        self.containers = []
        self.network = f"{PREFIX}-{run_id}"
        self.volume = None

    def name(self, role):
        name = f"{PREFIX}-{self.id}-{role}"
        self.containers.append(name)
        return name

    def close(self):
        for name in reversed(self.containers):
            subprocess.run(["docker", "rm", "-f", "-v", name], capture_output=True)
        if self.volume:
            subprocess.run(["docker", "volume", "rm", "-f", self.volume], capture_output=True)
        subprocess.run(["docker", "network", "rm", self.network], capture_output=True)


def wait_ready(port, timeout, path="/readyz"):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=2) as r:
                if r.status == 200:
                    return True
        except OSError:
            pass
        time.sleep(0.5)
    return False


def k6(run, out_dir, docs, env, cpu):
    os.makedirs(out_dir, exist_ok=True)
    # As the bench's own user: on a Linux host the run directory is not writable by k6's image user (a macOS Docker VM
    # maps it).
    args = [
        "docker",
        "run",
        "--rm",
        "--name",
        run.name(f"k6-{os.path.basename(out_dir)}"),
        "--network",
        run.network,
        "--user",
        f"{os.getuid()}:{os.getgid()}",
        "--cpuset-cpus",
        str(cpu),
        "-v",
        f"{DOCS}:/docs:ro",
        "-v",
        f"{out_dir}:/out",
        "-v",
        f"{BENCH}:/bench:ro",
        "-e",
        "K6_AUTO_EXTENSION_RESOLUTION=false",
        "-e",
        "K6_CSV_TIME_FORMAT=unix_milli",
        "-e",
        "TARGET=http://service:8080",
        "-e",
        "DOCS=" + ",".join(f"{d['name']}=/docs/{d['path']}" for d in docs),
    ]
    for key, value in env.items():
        args += ["-e", f"{key}={value}"]
    args += [K6_IMAGE, "run", "--quiet", "--no-color", "/bench/load.js"]
    if os.path.basename(out_dir) == "steady":
        args += ["--out", "csv=/out/k6.csv.gz"]
    print("+ k6", os.path.basename(out_dir), env, file=sys.stderr, flush=True)
    result = subprocess.run(args, capture_output=True, text=True)
    with open(os.path.join(out_dir, "k6.log"), "w") as f:
        f.write(result.stdout + result.stderr)
    # 99: a threshold with abortOnFail stopped a ramp, which is how a ramp ends.
    if result.returncode not in (0, 99):
        raise RuntimeError(f"k6 failed ({result.returncode}): {result.stderr.strip()[-2000:]}")
    return result.returncode


def cell(args):
    args.image = args.image or SUBJECTS[args.subject].get("image") or oracle_image()
    docs = select_docs(args.docs)
    cpus = args.cpus or args.pool
    n = vm_cpus()
    if cpus > n - 2:
        sys.exit(f"--cpus {cpus}: the VM has {n}; two are kept for k6, the sampler and the Collector")
    stamp = datetime.datetime.now().strftime("%Y%m%dT%H%M%S")
    out = args.out or os.path.join(RUNS, "adhoc", f"{args.name}-{stamp}")
    if os.path.exists(os.path.join(out, "done")):
        print(f"{out}: done already", file=sys.stderr)
        return 0
    os.makedirs(out, exist_ok=True)
    run = Run(f"{args.name}-{stamp}".lower().replace("_", "-")[:60])
    largest = max(int(d["bytes"]) for d in docs)
    subject = SUBJECTS[args.subject]
    meta = {
        "name": args.name,
        "subject": args.subject,
        "started": stamp,
        "docs": [d["name"] for d in docs],
        "pool": args.pool,
        "cpus": cpus,
        "memory": args.memory,
        "tmp": args.tmp,
        "tmpfs_size": args.tmpfs_size,
        "tracing": args.tracing,
        "mode": args.mode,
        "rate": args.rate,
        "vus": args.vus,
        "stages": args.stages,
        "duration": args.duration,
        "conversions": args.conversions,
        "io_bps": args.io_bps,
        "env": args.env,
        "image": args.image,
        "vm_cpus": n,
        "image_id": sh("docker", "image", "inspect", "--format", "{{.Id}}", args.image, quiet=True),
        "k6_image": K6_IMAGE,
        "collector_image": COLLECTOR_IMAGE if args.tracing == "on" else None,
    }
    meta["provenance"] = provenance_of(args, docs, n, cpus)
    meta["preflight"] = preflight()
    if cpu_some_avg10(meta["preflight"]) > args.max_vm_cpu_pressure:
        meta["warning"] = f"VM cpu pressure some avg10 above {args.max_vm_cpu_pressure} % before the run"
        print("warning:", meta["warning"], file=sys.stderr)
    # The 1-minute load still holds the previous run's VM threads, so only a load above every host cpu warns.
    if meta["preflight"]["host_loadavg"][0] > meta["preflight"]["host_cpus"]:
        load, host_cpus = meta["preflight"]["host_loadavg"][0], meta["preflight"]["host_cpus"]
        meta["host_warning"] = (
            f"host load {load:.1f} on {host_cpus} cpus before the run: something outside the VM competes for its vCPUs"
        )
        print("warning:", meta["host_warning"], file=sys.stderr)

    stop = []
    previous = signal.signal(signal.SIGINT, lambda *_: stop.append(True))
    signal.signal(signal.SIGTERM, lambda *_: stop.append(True))
    try:
        sh("docker", "network", "create", run.network)
        if args.subject == "oracle":
            limit = f"{max(largest * 2, 300 * 1024 * 1024) // 2**20}MB"
            env = {
                "POOL_SIZE": str(args.pool),
                "SPRING_SERVLET_MULTIPART_MAX_FILE_SIZE": limit,
                "SPRING_SERVLET_MULTIPART_MAX_REQUEST_SIZE": limit,
                "SERVER_TOMCAT_MAX_HTTP_FORM_POST_SIZE": limit,
                "SERVER_TOMCAT_CONNECTION_TIMEOUT": "120000",
            }
        elif args.subject == "gotenberg":
            if args.pool != 1:
                raise RuntimeError("gotenberg/gotenberg runs one LibreOffice per container: pool 1 only")
            env = {"API_PORT": "8080", "API_TIMEOUT": "900s", "LIBREOFFICE_AUTO_START": "true"}
        else:
            env = {
                "DOCX_TO_PDF_POOL_SIZE": str(args.pool),
                "DOCX_TO_PDF_MAX_DOCUMENT_SIZE": str(max(largest * 2, 300 * 1024 * 1024)),
                "DOCX_TO_PDF_CONVERSION_TIMEOUT": args.conversion_timeout,
                "DOCX_TO_PDF_POOL_MAX_WAITING": str(args.max_waiting),
                "DOCX_TO_PDF_POOL_WAIT_TIMEOUT": args.wait_timeout,
            }
        if args.tracing == "on" and args.subject != "ours":
            raise RuntimeError(f"tracing on is oxidestack/docx-to-pdf only, not {args.subject}")
        if args.tracing == "on":
            collector = run.name("collector")
            sh(
                "docker",
                "run",
                "-d",
                "--name",
                collector,
                "--network",
                run.network,
                "--network-alias",
                "collector",
                "--cpuset-cpus",
                str(n - 2),
                "-v",
                f"{BENCH}/otelcol.yaml:/etc/otelcol.yaml:ro",
                COLLECTOR_IMAGE,
                "--config",
                "/etc/otelcol.yaml",
            )
            env.update(
                {
                    "OTEL_EXPORTER_OTLP_ENDPOINT": "http://collector:4317",
                    "OTEL_EXPORTER_OTLP_PROTOCOL": "grpc",
                    "OTEL_TRACES_SAMPLER": "always_on",
                }
            )
        elif args.subject == "ours":
            env["OTEL_SDK_DISABLED"] = "true"
        for pair in args.env:
            key, _, value = pair.partition("=")
            env[key] = value
        meta["service_env"] = env

        service = run.name("service")
        docker = [
            "docker",
            "run",
            "-d",
            "--name",
            service,
            "--network",
            run.network,
            "--network-alias",
            "service",
            "-p",
            "127.0.0.1::8080",
            "--cpuset-cpus",
            f"0-{cpus - 1}",
            "--memory",
            args.memory,
            "--memory-swap",
            args.memory,
        ]
        if subject["secure"]:
            docker += [
                "--user",
                "10001:10001",
                "--read-only",
                "--cap-drop",
                "ALL",
                "--security-opt",
                "no-new-privileges",
            ]
        if args.tmp == "tmpfs":
            docker += ["--tmpfs", f"/tmp:rw,mode=1777,size={args.tmpfs_size}"]
        else:
            run.volume = f"{PREFIX}-{run.id}-tmp"
            docker += ["--mount", f"type=volume,src={run.volume},dst=/tmp"]
        if args.io_bps:
            docker += [
                "--device-read-bps",
                f"{args.io_device}:{args.io_bps}",
                "--device-write-bps",
                f"{args.io_device}:{args.io_bps}",
            ]
        for key, value in env.items():
            docker += ["-e", f"{key}={value}"]
        container_id = sh(*docker, args.image)
        port = sh("docker", "port", service, "8080/tcp").splitlines()[0].rsplit(":", 1)[1]
        began = time.time()
        if not wait_ready(port, args.ready_timeout, subject["ready"]):
            # Keep what it said and what ran (0049: moalhaddar/docx-to-pdf, pool above vCPU, 6 of 28 never ready in 300
            # s).
            os.makedirs(out, exist_ok=True)
            with open(os.path.join(out, "not-ready.log"), "w") as f:
                for command in (
                    ["docker", "logs", "--timestamps", service],
                    ["docker", "top", service],
                    ["docker", "inspect", "--format", "{{json .State}}", service],
                ):
                    done = subprocess.run(command, capture_output=True, text=True)
                    f.write(f"$ {' '.join(command)}\n{done.stdout}{done.stderr}\n")
            raise RuntimeError("the service never became ready")
        meta["ready_seconds"] = round(time.time() - began, 2)

        sampler = run.name("sampler")
        sh(
            "docker",
            "run",
            "-d",
            "--name",
            sampler,
            "--network",
            run.network,
            "--pid",
            f"container:{service}",
            "--cpuset-cpus",
            str(n - 1),
            "-v",
            "/sys/fs/cgroup:/host-cgroup:ro",
            "-v",
            f"{out}:/out",
            "-v",
            f"{BENCH}:/bench:ro",
            TOOLS_IMAGE,
            "python",
            "/bench/sampler.py",
            "--cgroup",
            f"/host-cgroup/{cgroup_dir(container_id)}",
            "--metrics",
            "http://service:8080/metrics",
            "--out",
            "/out/samples.jsonl",
        )

        # The route and the file field; k6's environment, below.
        endpoint = {"URL_PATH": subject.get("path", "/pdf"), "FIELD": subject.get("field", "document")}
        warm = max(args.warmup_per_instance * args.pool * len(docs), args.pool)
        if not stop:
            meta["warmup_started"] = time.time()
            k6(
                run,
                os.path.join(out, "warmup"),
                docs,
                {"MODE": "warmup", "VUS": str(args.pool), "ITERATIONS": str(warm), **endpoint},
                n - 1,
            )
        load = {
            "MODE": args.mode,
            "DURATION": args.duration,
            "TIMEOUT": args.request_timeout,
            "MAX_VUS": str(args.max_vus),
            **endpoint,
        }
        if args.mode in ("open", "ramp"):
            # A VU created mid-run takes long enough (it opens every document) that k6 drops an iteration meanwhile,
            # which ends a ramp (0069: 16 preallocated, the 17th at 110 % of saturation); a ramp starts with half
            # the cap.
            load["PRE_VUS"] = str(
                min(args.max_vus, max(4, args.pool * 4)) if args.mode == "open" else args.max_vus // 2
            )
        if args.mode == "open":
            load["RATE"] = str(args.rate)
        elif args.mode == "closed" and args.conversions:
            # Whole rounds of the documents, so a mix of sizes is not cut short of its slow ones.
            rounds = -(-args.conversions // len(docs))
            load.update(
                {
                    "MODE": "count",
                    "VUS": str(args.vus or args.pool + 2),
                    "ITERATIONS": str(rounds * len(docs)),
                    "DURATION": "60m",
                }
            )
        elif args.mode == "closed":
            load["VUS"] = str(args.vus or args.pool + 2)
        elif args.mode == "ramp":
            load["STAGES"] = args.stages
        if not stop:
            meta["steady_started"] = time.time()
            meta["k6_exit"] = k6(run, os.path.join(out, "steady"), docs, load, n - 1)
            meta["steady_ended"] = time.time()
        if args.recovery and not stop and args.mode == "ramp":
            k6(
                run,
                os.path.join(out, "recovery"),
                docs,
                {
                    "MODE": "open",
                    "RATE": str(args.recovery),
                    "DURATION": "2m",
                    "TIMEOUT": args.request_timeout,
                    **endpoint,
                },
                n - 1,
            )

        sh("docker", "stop", "-t", "15", sampler)
        state = json.loads(sh("docker", "inspect", "--format", "{{json .State}}", service, quiet=True))
        meta["service_state"] = {k: state.get(k) for k in ("Status", "OOMKilled", "ExitCode", "RestartCount")}
        logs = subprocess.run(["docker", "logs", "--tail", "5000", service], capture_output=True, text=True)
        with open(os.path.join(out, "service.log"), "w") as f:
            f.write(logs.stdout + logs.stderr)
    except Exception as error:
        meta["error"] = str(error)
        raise
    finally:
        meta["interrupted"] = bool(stop)
        run.close()
        meta["ended"] = time.time()
        meta["host_loadavg_end"] = os.getloadavg()
        with open(os.path.join(out, "run.json"), "w") as f:
            json.dump(meta, f, indent=1)
        signal.signal(signal.SIGINT, previous)
    if stop:
        sys.exit("interrupted; the run's containers are removed")
    with open(os.path.join(out, "done"), "w") as f:
        f.write(stamp + "\n")
    print(f"{out}: done", file=sys.stderr)
    if args.cooldown:
        time.sleep(args.cooldown)
    return 0


def cell_options(parser):
    parser.add_argument("--docs", required=True, help="document names or class:<class>, comma-separated")
    parser.add_argument("--subject", choices=sorted(SUBJECTS), default="ours")
    parser.add_argument("--image", help="default: docx-to-pdf:bench for ours, oracle.env's for the oracle")
    parser.add_argument("--pool", type=int, default=1)
    parser.add_argument("--cpus", type=int, help="the service's cpuset size (default: the pool size)")
    parser.add_argument(
        "--memory", default="16g", help="the service's memory limit, swap equal, so swap cannot hide an OOM"
    )
    parser.add_argument("--tmp", choices=("disk", "tmpfs"), default="disk")
    parser.add_argument("--tmpfs-size", default="2g")
    parser.add_argument("--tracing", choices=("off", "on"), default="off")
    parser.add_argument("--mode", choices=("open", "closed", "ramp"), default="closed")
    parser.add_argument("--rate", type=float, help="open: requests per second")
    parser.add_argument("--vus", type=int, help="closed: clients (default: pool + 2)")
    parser.add_argument("--stages", help="ramp: rate:duration,...")
    parser.add_argument("--duration", default="5m")
    parser.add_argument(
        "--conversions",
        type=int,
        help="closed: run until this many requests (rounded up to whole rounds of the documents) "
        "instead of for --duration",
    )
    parser.add_argument(
        "--warmup-per-instance",
        type=int,
        default=2,
        help="warm-up conversions per instance and document (a first conversion is slower)",
    )
    parser.add_argument("--recovery", type=float, help="ramp: then 2 minutes at this rate")
    parser.add_argument("--io-bps", help="read and write limit on the VM disk, e.g. 120mb")
    parser.add_argument("--io-device", default="/dev/vdb", help="the VM disk Docker's volumes live on")
    parser.add_argument("--conversion-timeout", default="600s")
    parser.add_argument("--wait-timeout", default="600s")
    parser.add_argument("--max-waiting", type=int, default=1000)
    parser.add_argument("--request-timeout", default="1500s")
    parser.add_argument("--max-vus", type=int, default=200)
    parser.add_argument("--ready-timeout", type=float, default=300)
    parser.add_argument("--env", action="append", default=[], help="KEY=VALUE for the service, repeatable")
    parser.add_argument("--cooldown", type=float, default=15, help="seconds between runs")
    parser.add_argument("--max-vm-cpu-pressure", type=float, default=5.0)
    parser.add_argument("--out", help=argparse.SUPPRESS)


def matrix(args):
    """FILE is a TSV whose header names cell options (`cell`, `docs`, `pool`, `tmp`, ...); one row a cell."""
    with open(args.file) as f:
        rows = [r for r in csv.DictReader((line for line in f if not line.startswith("#")), delimiter="\t")]
    only = set(args.only.split(",")) if args.only else None
    name = os.path.splitext(os.path.basename(args.file))[0]
    runs = os.path.abspath(args.runs) if args.runs else RUNS
    plan = [(row, rep) for row in rows for rep in range(1, args.reps + 1) if not only or row["cell"] in only]
    random.Random(args.seed).shuffle(plan)
    total = len(plan)
    for i, (row, rep) in enumerate(plan, 1):
        out = os.path.join(runs, name, row["cell"], f"r{rep}")
        if os.path.exists(os.path.join(out, "done")):
            continue
        argv = [row["cell"]]
        for key, value in row.items():
            if key == "cell" or value in ("", None):
                continue
            for v in value.split(";") if key == "env" else [value]:
                argv += [f"--{key.replace('_', '-')}", v]
        parser = argparse.ArgumentParser()
        parser.add_argument("name")
        cell_options(parser)
        cell_args = parser.parse_args(argv)
        cell_args.out = out
        cell_args.image = args.image or cell_args.image
        cell_args.subject = args.subject or cell_args.subject
        cell_args.cell_command = shlex.join(["tools/bench/bench.py", "cell", *argv])
        cell_args.matrix = os.path.relpath(os.path.abspath(args.file), ROOT)
        cell_args.matrix_seed = args.seed
        cell_args.replicate, cell_args.replicates = rep, args.reps
        print(f"[{i}/{total}] {row['cell']} r{rep}", file=sys.stderr, flush=True)
        try:
            cell(cell_args)
        except RuntimeError as error:
            print(f"{row['cell']} r{rep} failed: {error}", file=sys.stderr)
    return 0


def tool(script, *script_args, mounts=()):
    args = [
        "docker",
        "run",
        "--rm",
        "--name",
        f"{PREFIX}-{script.split('.')[0]}-{os.getpid()}",
        "-v",
        f"{ROOT}:/repo:ro",
        "-v",
        f"{DATA}:/data",
    ]
    for host, inside in mounts:
        args += ["-v", f"{host}:{inside}"]
    args += ["-w", "/repo/tools/bench", TOOLS_IMAGE, "python", script, *script_args]
    return subprocess.run(args).returncode


PROBE_DOCS = "report-img30-10mb,report-img45-12mb,report-img60-15mb,report-img60-20mb"


def probe_plan(cpus):
    """bench.py probe's two count runs on a pool of `cpus` instances on `cpus` vCPU: `alone`, one client, five rounds
    over PROBE_DOCS; `busy`, pool + 2 clients, so two requests always wait in the pool's queue and an instance
    starts its next conversion as it ends the last, never idle while a client uploads or downloads (with pool-many
    clients the instances idled during the transfers, and S_sat counted them)."""
    rounds = len(PROBE_DOCS.split(","))
    return {
        "alone": {"vus": 1, "conversions": rounds * 5},
        "busy": {"vus": cpus + PROBE_QUEUE, "conversions": 120 + 3 * cpus},
    }


PROBE_QUEUE = 2  # requests waiting in the busy phase; the closed-loop cells' default (pool + 2)
PROBE_OCCUPANCY = 0.93  # the busy run's instance occupancy below which instances idled: a warning (0078: 0.89 with
# pool-many clients, 0.96-0.98 with a queue)


def probe_numbers(cpus, alone, busy):
    """s1 and S_sat from the probe's two summaries (analyze.py). s1: the mean latency alone, as a client sees it.
    S_sat: pool / X, X the busy run's throughput where every client is busy; with a queue every instance converts
    all along, so pool / X is an instance's time per conversion with every vCPU converting, as sizing.py's
    X = pool / S_sat reads it. `busy` carries what shows the instances were kept converting: `occupancy`, the
    share of the time an instance spent converting (the server's mean conversion time × X / pool); `cpu_steady`,
    the vCPU share in use where X is counted (CPU-seconds a conversion × X / vCPU; `cpu_utilisation` is over the
    whole run, its first round and drain included); the server's mean pool wait (above 0: requests queued) and
    conversion time. A conversion with every vCPU busy uses about 0.95 of one (0078), so cpu_steady tops out
    under 1 while occupancy reaches 0.96-0.98."""
    s_sat = cpus / busy["throughput"]
    per, conversion = busy.get("cpu_seconds_per_conversion"), busy.get("server_conversion_mean_s")
    check = {
        "vus": busy.get("vus"),
        "occupancy": conversion * busy["throughput"] / cpus if conversion is not None else None,
        "cpu_steady": per * busy["throughput"] / cpus if per is not None else None,
        "cpu_utilisation": busy.get("cpu_utilisation"),
        "server_wait_mean_s": busy.get("server_wait_mean_s"),
        "server_conversion_mean_s": conversion,
    }
    warnings = []
    if (busy.get("vus") or 0) <= cpus:
        warnings.append(
            f"the busy run had {busy.get('vus')} clients for {cpus} instances: no queue, so S_sat "
            "includes the transfers"
        )
    if check["occupancy"] is not None and check["occupancy"] < PROBE_OCCUPANCY:
        warnings.append(
            f"the busy run's instances converted {check['occupancy']:.2f} of the time (under "
            f"{PROBE_OCCUPANCY}): they idled between conversions"
        )
    check = {key: round(v, 3) if isinstance(v, float) else v for key, v in check.items()}
    return {
        "s1": alone["latency"]["mean"],
        "s_sat": s_sat,
        "contention": s_sat / alone["latency"]["mean"],
        "busy": check,
        "warnings": warnings,
    }


def probe_url(args):
    """The platform's three numbers from a running service's probe endpoint (DOCX_TO_PDF_PROBE_PORT):
    POSTs the reference mix (PROBE_DOCS from .bench/docs/) to URL/probe and saves the answer, explain()'s JSON,
    measured by the service on its own node under its own limits. In Kubernetes: `kubectl port-forward pod/P
    9090:9090`, then `--url http://localhost:9090`."""
    import urllib.error
    import urllib.parse

    docs = select_docs(PROBE_DOCS)
    boundary = f"bench-probe-{os.getpid()}"
    body = bytearray()
    for row in docs:
        with open(os.path.join(DOCS, row["path"]), "rb") as f:
            content = f.read()
        body += (
            f'--{boundary}\r\nContent-Disposition: form-data; name="document"; '
            f'filename="{row["name"]}.docx"\r\nContent-Type: application/octet-stream\r\n\r\n'
        ).encode()
        body += content + b"\r\n"
    body += f"--{boundary}--\r\n".encode()
    query = {"disk_mib": args.disk_mib}
    for key in ("rounds", "busy", "cpus", "name"):
        if getattr(args, key) is not None:
            query[key] = getattr(args, key)
    base = args.url.rstrip("/")
    url = (base if base.endswith("/probe") else base + "/probe") + "?" + urllib.parse.urlencode(query)
    request = urllib.request.Request(
        url, data=bytes(body), method="POST", headers={"Content-Type": f"multipart/form-data; boundary={boundary}"}
    )
    print(
        f"probe: POST {url}, {len(docs)} documents, {len(body) / 2**20:.1f} MiB; the service is drained meanwhile",
        file=sys.stderr,
        flush=True,
    )
    try:
        with urllib.request.urlopen(request, timeout=args.timeout) as answer:
            out = json.load(answer)
    except urllib.error.HTTPError as error:
        sys.exit(f"probe: {error.code} {error.read().decode(errors='replace')}")
    stamp = datetime.datetime.now().strftime("%Y%m%dT%H%M%S")
    path = args.out or os.path.join(RUNS, "probe", stamp, "platform.json")
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w") as f:
        json.dump(out, f, indent=1, ensure_ascii=False)
    print(json.dumps(out, indent=1, ensure_ascii=False))
    print(f"bench.py size hardware --platform {os.path.relpath(path)} --cpus C --memory GIB", file=sys.stderr)
    return 0


def probe(args):
    """The platform's three numbers for sizing.py, measured on this Docker host in a few minutes:
    s1, a conversion's mean time with nothing else running (one client); contention, S_sat / s1 with every one of
    the pod's vCPU converting (probe_plan, probe_numbers); the disk's MB/s, 1 MiB direct writes to a volume, as
    LibreOffice writes /tmp."""
    n = int(sh("docker", "info", "--format", "{{.NCPU}}"))
    cpus = args.cpus or max(1, min(4, n - 2))
    stamp = datetime.datetime.now().strftime("%Y%m%dT%H%M%S")
    base = os.path.join(RUNS, "probe", stamp)
    plan = probe_plan(cpus)
    for name, opts in plan.items():
        parser = argparse.ArgumentParser()
        parser.add_argument("name")
        cell_options(parser)
        cell_args = parser.parse_args(
            [
                f"probe-{name}",
                "--docs",
                PROBE_DOCS,
                "--pool",
                str(cpus),
                "--cpus",
                str(cpus),
                "--vus",
                str(opts["vus"]),
                "--conversions",
                str(opts["conversions"]),
                "--warmup-per-instance",
                "1",
                "--subject",
                args.subject,
            ]
        )
        cell_args.out = os.path.join(base, name)
        cell_args.image = args.image or cell_args.image
        print(f"probe: {name}, {opts['vus']} client(s), {opts['conversions']} conversions", file=sys.stderr, flush=True)
        cell(cell_args)
    if tool(
        "analyze.py",
        *[f"/data/{os.path.relpath(os.path.join(base, x), DATA)}" for x in plan],
        "--out",
        f"/data/{os.path.relpath(os.path.join(base, 'report'), DATA)}",
    ):
        sys.exit("probe: the analysis failed")
    alone, busy = (json.load(open(os.path.join(base, x, "summary.json"))) for x in plan)
    numbers = probe_numbers(cpus, alone, busy)
    s1, s_sat = numbers["s1"], numbers["s_sat"]
    for warning in numbers["warnings"]:
        print("warning:", warning, file=sys.stderr)
    volume = f"{PREFIX}-probe-{os.getpid()}"
    try:
        dd = subprocess.run(
            [
                "docker",
                "run",
                "--rm",
                "--name",
                volume,
                "-v",
                f"{volume}:/t",
                TOOLS_IMAGE,
                "dd",
                "if=/dev/zero",
                "of=/t/probe",
                "bs=1M",
                f"count={args.disk_mib}",
                "oflag=direct",
            ],
            capture_output=True,
            text=True,
        )
    finally:
        subprocess.run(["docker", "volume", "rm", "-f", volume], capture_output=True)
    seconds = None
    for line in dd.stderr.splitlines():  # "2147483648 bytes (2.1 GB, 2.0 GiB) copied, 9.8 s, 219 MB/s"
        if "copied" in line:
            seconds = float(line.split(",")[2].split()[0])
    model = sh(
        "docker",
        "run",
        "--rm",
        "--name",
        f"{PREFIX}-cpuinfo-{os.getpid()}",
        TOOLS_IMAGE,
        "sh",
        "-c",
        "grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 || uname -m",
        quiet=True,
    ).strip()
    import sizing

    k = sizing.load(
        args.coefficients,
        s1=round(s1, 3),
        contention=round(s_sat / s1, 3),
        disk_mbps=round(args.disk_mib * 2**20 / seconds / 1e6, 1) if seconds else None,
    )
    k["hw"].update(
        platform=args.name or f"{model or 'unknown CPU'}, {n} vCPU", measured=f"bench.py probe, {stamp}", cpus=cpus
    )
    out = sizing.explain(k, cpus)
    out["platform"].update(
        runs=os.path.relpath(base, ROOT),
        s_sat=round(s_sat, 3),
        by_document_s={d: round(v["mean"], 3) for d, v in (alone.get("latency_by_doc") or {}).items()},
        busy=numbers["busy"],
    )
    with open(args.out or os.path.join(base, "platform.json"), "w") as f:
        json.dump(out, f, indent=1, ensure_ascii=False)
    print(json.dumps(out, indent=1, ensure_ascii=False))
    print(
        f"bench.py size hardware --platform {os.path.relpath(args.out or os.path.join(base, 'platform.json'))} "
        "--cpus C --memory GIB",
        file=sys.stderr,
    )
    return 0


def size(args):
    """The calculator: either question (this hardware's settings, or the pods a target needs) from the measured
    coefficients."""
    import sizing

    k = sizing.load(args.coefficients, args.platform, s1=args.s1, contention=args.contention)
    hw = k["hw"]
    basis = "typical" if args.typical else "worst"
    case = "typical mix" if args.typical else "worst case"
    print(
        f"workload: {k['workload']['name']} ({k['measured']}); platform: {hw['platform']}: s1 {hw['s1']:g} s, "
        f"contention {hw['contention']:g}" + (f", disk {hw['disk_mbps']:g} MB/s" if hw.get("disk_mbps") else ""),
        file=sys.stderr if args.json or args.question == "explain" else sys.stdout,
    )
    if args.question == "explain":
        print(json.dumps(sizing.explain(k, args.cpus or 4), indent=1, ensure_ascii=False))
        return 0
    if args.question == "hardware":
        if not args.cpus or not args.memory:
            sys.exit("size hardware needs --cpus and --memory")
        answer = sizing.hardware(k, args.cpus, args.memory, args.disk_mbps, args.tmpfs, basis)
        if args.json or "error" in answer:
            print(json.dumps(answer, indent=1))
            return 0 if "error" not in answer else 1
        disk = f", disk {answer['disk_mbps']:g} MB/s" if answer["disk_mbps"] and not args.tmpfs else ""
        print(f"{args.cpus} vCPU, {args.memory:g} GiB{', tmpfs' if args.tmpfs else ''}{disk}:")
        print(
            f"  pool {answer['pool']} (CPU allows {answer['pool_by_cpu']}, memory {answer['pool_by_memory']}; "
            f"bound by {answer['bound']})"
        )
        print(f"  throughput {answer['throughput_per_s']:.2f}/s = {answer['throughput_per_min']:.0f}/min at saturation")
        print(
            f"  a conversion at saturation: mean {answer['service_mean_s']:.1f} s, "
            f"p95 {answer['service_p95_s']:.1f} s, p99 {answer['service_p99_s']:.1f} s"
        )
        print(f"  memory limit {answer['memory_limit_gib']:.2f} GiB (request = limit, {case})")
        print(
            f"  /tmp {answer['tmp_mib']:g} MiB ({case})"
            + (", a tmpfs counted in the memory limit" if args.tmpfs else ", on disk")
        )
        for key, value in answer["settings"].items():
            print(f"  {key}={value}")
        return 0
    if not args.latency or not args.rate:
        sys.exit("size target needs --latency and --rate")
    number, _, unit = args.rate.partition("/")
    rate = float(number) / {"": 1, "s": 1, "min": 60, "h": 3600}[unit]
    options = sizing.target(k, args.latency, rate, args.quantile, basis=basis)
    if args.json:
        print(json.dumps(options, indent=1))
        return 0
    print(f"p{args.quantile * 100:g} ≤ {args.latency:g} s at {rate:.3f}/s ({rate * 60:.1f}/min):")
    q = "p" + format(args.quantile * 100, "g")
    print(f"  {'vCPU':>4} {'pool':>4} {'pods':>4} {'total vCPU':>10} {'GiB/pod':>7} {'load':>5} {q:>6}")
    for o in options:
        if o["pods"] is None:
            print(f"  {o['cpus']:>4} {o['pool']:>4}    -  {o['why']}")
            continue
        print(
            f"  {o['cpus']:>4} {o['pool']:>4} {o['pods']:>4} {o['total_vcpu']:>10} {o['memory_gib']:>7.2f} "
            f"{o['utilisation'] * 100:>4.0f}% {o['latency_s']:>5.1f}s"
        )
    best = next((o for o in options if o["pods"]), None)
    if best:
        print(
            f"  first row: {best['pods']} × ({best['cpus']} vCPU, {best['memory_gib']:.2f} GiB); "
            "add one pod for N+1. Settings:"
        )
        for key, value in best["settings"].items():
            print(f"  {key}={value}")
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("tools")
    g = sub.add_parser("generate")
    g.add_argument("--hostile", action="store_true")
    g.add_argument("--heavy", action="store_true", help="the heavy documents to 1.1 GB and the content bombs")
    g.add_argument("--only")
    c = sub.add_parser("cell")
    c.add_argument("name")
    cell_options(c)
    m = sub.add_parser("matrix")
    m.add_argument("file")
    m.add_argument("--reps", type=int, default=3)
    m.add_argument("--seed", type=int, default=1)
    m.add_argument("--only")
    m.add_argument("--image")
    m.add_argument("--subject", choices=sorted(SUBJECTS), help="overrides the matrix's (default ours)")
    m.add_argument("--runs", help="the runs' directory, under .bench/ (default .bench/runs)")
    r = sub.add_parser("report")
    r.add_argument("runs", nargs="*")
    r.add_argument("--out", default=os.path.join(DATA, "report"), help="under .bench/ (default .bench/report)")
    r.add_argument("--against", help="another report's directory under .bench/: each cell's figures judged at 2σ")
    sub.add_parser("preflight")
    pr = sub.add_parser("probe")
    pr.add_argument(
        "--cpus",
        type=int,
        help="the pod's vCPU (default: min(4, the VM's - 2); with --url, the "
        "vCPU the formulas are worked out for, default the pool)",
    )
    pr.add_argument("--subject", choices=sorted(SUBJECTS), default="ours")
    pr.add_argument("--image")
    pr.add_argument("--name", help="the platform's name (default: the CPU model)")
    pr.add_argument("--disk-mib", type=int, default=2048, help="the disk test's size")
    pr.add_argument("--out", help="the platform JSON (default: .bench/runs/probe/<stamp>/platform.json)")
    pr.add_argument("--coefficients", help="the workload whose formulas the answer explains (default: amd64's)")
    pr.add_argument("--url", help="a running service's probe endpoint (DOCX_TO_PDF_PROBE_PORT), instead of Docker")
    pr.add_argument("--rounds", type=int, help="--url: rounds over the documents, one at a time (default 5)")
    pr.add_argument("--busy", type=int, help="--url: conversions one per instance at once (default 120 + 3 x pool)")
    pr.add_argument("--timeout", type=float, default=1800, help="--url: seconds to wait for the answer")
    z = sub.add_parser("size")
    z.add_argument("question", choices=("hardware", "target", "explain"))
    z.add_argument("--coefficients", help="the workload; default: tools/bench/coefficients/amd64-report.json")
    z.add_argument(
        "--platform",
        help="a probe's platform.json, or arm64 / amd64 (a quick plan's report); default: the workload's own",
    )
    z.add_argument("--s1", type=float, help="seconds a conversion takes alone (overrides the platform's)")
    z.add_argument("--contention", type=float, help="S with every vCPU busy / s1 (default 1.2 with --s1 alone)")
    z.add_argument("--cpus", type=int)
    z.add_argument("--memory", type=float, help="GiB")
    z.add_argument("--disk-mbps", type=float, help="the disk's sustained MB/s, /tmp on it")
    z.add_argument("--tmpfs", action="store_true", help="/tmp on a tmpfs, counted in memory")
    z.add_argument("--latency", type=float, help="the response-time target, seconds")
    z.add_argument("--quantile", type=float, default=0.95)
    z.add_argument("--rate", help="requests per second, or N/min, N/h")
    z.add_argument(
        "--typical",
        action="store_true",
        help="size memory and /tmp from the typical mix's fit, not the worst case (sizing.py basis)",
    )
    z.add_argument("--json", action="store_true")
    args = parser.parse_args()

    if args.command == "tools":
        return subprocess.run(["docker", "buildx", "build", "--load", "-t", TOOLS_IMAGE, BENCH]).returncode
    if args.command == "generate":
        extra = (
            (["--hostile"] if args.hostile else [])
            + (["--heavy"] if args.heavy else [])
            + (["--only", args.only] if args.only else [])
        )
        return tool(
            "generate.py", "/data/docs", *extra, mounts=[(FIXTURES, "/fixtures:ro")] if os.path.isdir(FIXTURES) else []
        )
    if args.command == "cell":
        return cell(args)
    if args.command == "matrix":
        return matrix(args)
    if args.command == "report":
        runs = [os.path.relpath(os.path.abspath(p), DATA) for p in args.runs] or ["runs"]
        out = os.path.relpath(os.path.abspath(args.out), DATA)
        if out.startswith(".."):
            sys.exit("--out must be under .bench/")
        against = []
        if args.against:
            other = os.path.relpath(os.path.abspath(args.against), DATA)
            if other.startswith(".."):
                sys.exit("--against must be under .bench/")
            against = ["--against", f"/data/{other}"]
        return tool("analyze.py", *[f"/data/{p}" for p in runs], "--out", f"/data/{out}", *against)
    if args.command == "size":
        return size(args)
    if args.command == "probe":
        return probe_url(args) if args.url else probe(args)
    if args.command == "preflight":
        print(json.dumps(preflight(), indent=1))
        return 0
    return 2


if __name__ == "__main__":
    sys.exit(main())
