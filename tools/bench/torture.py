#!/usr/bin/env python3
"""The torture test. Hostile documents sent to oxidestack/docx-to-pdf, Gotenberg and
moalhaddar/docx-to-pdf, each service alone in a fresh container, and a soak of thousands of typical conversions.
Standard library only; runs on an amd64 Docker host (a cloud VM, never an M1 Max: moalhaddar/docx-to-pdf is amd64-only),
with bench.py beside it (the container's cgroup, systemd's or cgroupfs's). `make compare` drives it.

  torture.py hostile SUBJECT DOC [--alongside REPORT] [--cap 300] [--recover S] [--out DIR]
      DOC alone, with REPORT sent in a loop alongside it from 2 s in, and after DOC's answer until one converts
      (at most S seconds; once without --recover). Records the
      status and seconds of every request, the container cgroup's memory every 0.2 s, memory.peak and memory.events,
      and whether the container is still running and ready afterwards.
  torture.py one SUBJECT DOC [DOC ...] [--out DIR]
      each DOC in a fresh container, alone: status, seconds, memory.peak, the PDF and `pdfimages -list`.
  torture.py pause SUBJECT DOC [--pauses 2,2,2] [--stall 200] [--out DIR]
      DOC in pieces with pauses between them (it should convert); then a request that stops sending halfway (when,
      if ever, is it cut?).
  torture.py egress SUBJECT DOCS_DIR [--out DIR]
      image/egress's documents against a listener on two internal networks (image/egress-test.sh's set-up, any
      subject): did a connection arrive?
  torture.py soak SUBJECT DOC_DIR --manifest M --classes report,objects -n 3000 [--cpus 0] [--env K=V] [--out DIR]
      N conversions one at a time, cycling the documents; after each, every process's RSS and the cgroup's memory.
  torture.py post URL FIELD DOCS_DIR OUT_DIR     (inside a client container: egress's client for any field)

SUBJECT: ours (IMAGE_OURS, default docx-to-pdf:torture, defaults and the chart's security context), gotenberg (8.38.1 as
shipped: its port, route and field, no settings), moalhaddar (oracle.env's image, the multipart limits raised as
deployments raise them).
"""

import argparse
import json
import os
import random
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
GOTENBERG = "gotenberg/gotenberg:8.38.1@sha256:aa552cc115db488e0f0bcb07446aac13071ad51505b642c84bb894500907b42b"
ORACLE = "moalhaddar/docx-to-pdf@sha256:14497d940936fe346cf74ce9c65bfa1eaff71487371888bd864777fd63a386c3"
# The Docker host's /sys/fs/cgroup: mounted at /host-cgroup in make compare's runner (examples/compare/compose.yaml),
# the host's own when this runs on the Docker host. The container's directory under it is bench.cgroup_dir's.
CGROUP_ROOT = "/host-cgroup" if os.path.isdir("/host-cgroup") else "/sys/fs/cgroup"
PYTHON = "python@sha256:f6a589d43c42b9e7f7dc67a12d37132491f362859a5d750607710cc56da3bc72"
SUBJECTS = {
    "ours": {
        "image": os.environ.get("IMAGE_OURS", "docx-to-pdf:torture"),
        "port": 8080,
        "path": "/pdf",
        "field": "document",
        "ready": "/readyz",
        "env": {"OTEL_SDK_DISABLED": "true"},
        "secure": ["--user", "10001:10001", "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges"],
    },
    "gotenberg": {
        "image": GOTENBERG,
        "port": 3000,
        "path": "/forms/libreoffice/convert",
        "field": "files",
        "ready": "/health",
        "env": {},
        "secure": [],
    },
    "moalhaddar": {
        "image": ORACLE,
        "port": 8080,
        "path": "/pdf",
        "field": "document",
        "ready": "/actuator/health/readiness",
        "secure": [],
        "env": {
            "POOL_SIZE": "1",
            "SPRING_SERVLET_MULTIPART_MAX_FILE_SIZE": "300MB",
            "SPRING_SERVLET_MULTIPART_MAX_REQUEST_SIZE": "300MB",
            "SERVER_TOMCAT_MAX_HTTP_FORM_POST_SIZE": "300MB",
            "SERVER_TOMCAT_CONNECTION_TIMEOUT": "120000",
        },
    },
}


def sh(*cmd, check=True):
    r = subprocess.run(cmd, capture_output=True, text=True)
    if check and r.returncode != 0:
        raise RuntimeError(f"{' '.join(cmd)}: {r.stderr.strip()}")
    return r.stdout.strip()


def body_of(path, field, name=None):
    b = f"----torture{random.getrandbits(64):016x}"
    data = open(path, "rb").read()
    name = name or os.path.basename(path)
    head = (
        f'--{b}\r\nContent-Disposition: form-data; name="{field}"; filename="{name}"\r\n'
        "Content-Type: application/octet-stream\r\n\r\n"
    ).encode()
    return head + data + f"\r\n--{b}--\r\n".encode(), f"multipart/form-data; boundary={b}"


def post(url, path, field, timeout, name=None):
    body, ctype = body_of(path, field, name)
    t0 = time.time()
    rec = {"doc": name or os.path.basename(path), "t0": round(t0, 2)}
    try:
        req = urllib.request.Request(url, body, {"Content-Type": ctype})
        with urllib.request.urlopen(req, timeout=timeout) as r:
            rec["status"], out = r.status, r.read()
            rec["headers"] = {k: v for k, v in r.headers.items() if k.lower().startswith(("docx", "content-type"))}
    except urllib.error.HTTPError as e:
        rec["status"], out = e.code, e.read()
    except Exception as e:  # noqa: BLE001 - a timeout or a reset is the finding
        rec["status"], out = f"{type(e).__name__}: {e}"[:120], b""
    rec["seconds"] = round(time.time() - t0, 2)
    rec["bytes"] = len(out)
    if rec["status"] != 200:
        rec["body"] = out[:300].decode("latin-1")
    return rec, out


class Service:
    def __init__(self, subject, name, host_port, cpus="2-3", memory="12g", env=(), network=None):
        self.s = SUBJECTS[subject]
        self.subject, self.name, self.port = subject, name, host_port
        sh("docker", "rm", "-f", "-v", name, check=False)
        sh("docker", "volume", "rm", "-f", f"{name}-tmp", check=False)
        cmd = [
            "docker",
            "run",
            "-d",
            "--name",
            name,
            "--cpuset-cpus",
            cpus,
            "--memory",
            memory,
            "--memory-swap",
            memory,
            "--mount",
            f"type=volume,src={name}-tmp,dst=/tmp",
            *self.s["secure"],
        ]
        cmd += ["--network", network] if network else ["-p", f"127.0.0.1:{host_port}:{self.s['port']}"]
        envs = dict(self.s["env"])
        envs.update(dict(e.split("=", 1) for e in env))
        for k, v in envs.items():
            cmd += ["-e", f"{k}={v}"]
        self.env = envs
        t0 = time.time()
        self.id = sh(*cmd, self.s["image"])
        # Imported here, not at the top: the linked-picture client mounts this file alone in its container.
        from bench import cgroup_dir

        self.cgroup = f"{CGROUP_ROOT}/{cgroup_dir(self.id)}"
        self.started = t0

    @property
    def url(self):
        return f"http://127.0.0.1:{self.port}"

    def wait_ready(self, limit=300):
        while time.time() - self.started < limit:
            if self.ready():
                return round(time.time() - self.started, 2)
            time.sleep(0.1)
        return None

    def ready(self):
        try:
            with urllib.request.urlopen(self.url + self.s["ready"], timeout=3) as r:
                return r.status == 200
        except Exception:  # noqa: BLE001
            return False

    def cg(self, f):
        try:
            return open(f"{self.cgroup}/{f}").read()
        except OSError:
            return None

    def memory(self):
        cur, stat = self.cg("memory.current"), self.cg("memory.stat")
        anon = None
        if stat:
            anon = next((int(ln.split()[1]) for ln in stat.splitlines() if ln.startswith("anon ")), None)
        return (int(cur) if cur else None), anon

    def state(self):
        st = json.loads(sh("docker", "inspect", "-f", "{{json .State}}", self.name, check=False) or "{}")
        ev = self.cg("memory.events") or ""
        return {
            "status": st.get("Status"),
            "running": st.get("Running"),
            "oom_killed": st.get("OOMKilled"),
            "exit_code": st.get("ExitCode"),
            "ready": self.ready(),
            "memory_peak": int(self.cg("memory.peak") or 0) or None,
            "memory_events": dict((ln.split()[0], int(ln.split()[1])) for ln in ev.splitlines() if ln.strip()),
        }

    def processes(self):
        out = sh("docker", "top", self.name, "-eo", "pid,rss,args", check=False)
        rss = {}
        for line in out.splitlines()[1:]:
            parts = line.split(None, 2)
            if len(parts) == 3 and parts[1].isdigit():
                words = parts[2].split()
                key = os.path.basename(words[0])
                if key == "docx-to-pdf" and len(words) > 1 and words[1] == "instance":
                    key = "instance"
                rss[key] = rss.get(key, 0) + int(parts[1])
        return rss

    def remove(self, out=None):
        if out:
            r = subprocess.run(["docker", "logs", "--timestamps", self.name], capture_output=True, text=True)
            with open(os.path.join(out, f"{self.name}.log"), "w") as f:
                f.write(r.stdout + r.stderr)
        sh("docker", "rm", "-f", "-v", self.name, check=False)
        sh("docker", "volume", "rm", "-f", f"{self.name}-tmp", check=False)


def sampler(svc, stop, rows):
    t0 = time.time()
    while not stop.is_set():
        cur, anon = svc.memory()
        rows.append((round(time.time() - t0, 1), cur, anon))
        time.sleep(0.2)


def hostile(a):
    out = a.out
    os.makedirs(out, exist_ok=True)
    svc = Service(a.subject, f"docx-to-pdf-torture-{a.subject}", 18124)
    res = {
        "subject": a.subject,
        "image": svc.s["image"],
        "env": svc.env,
        "doc": os.path.basename(a.doc),
        "doc_bytes": os.path.getsize(a.doc),
        "alongside": os.path.basename(a.alongside),
        "cap": a.cap,
    }
    try:
        res["ready_s"] = svc.wait_ready()
        warm, _ = post(svc.url + svc.s["path"], a.alongside, svc.s["field"], a.cap)
        res["warmup"] = warm
        res["memory_after_warmup"] = svc.memory()
        stop, samples = threading.Event(), []
        threading.Thread(target=sampler, args=(svc, stop, samples), daemon=True).start()
        done, reports = threading.Event(), []

        def side():
            time.sleep(2)
            while not done.is_set():
                rec, _ = post(svc.url + svc.s["path"], a.alongside, svc.s["field"], a.cap)
                rec["during"] = True
                reports.append(rec)
                if rec["status"] != 200:
                    time.sleep(1)

        th = threading.Thread(target=side, daemon=True)
        th.start()
        bomb, _ = post(svc.url + svc.s["path"], a.doc, svc.s["field"], a.cap)
        done.set()
        res["hostile"] = bomb
        res["state_at_answer"] = svc.state()
        th.join(a.cap + 5)
        # After the hostile answer: the report again until one converts (how long the service stays unusable), or
        # until --recover seconds have passed.
        answered = time.time()
        while True:
            after, _ = post(svc.url + svc.s["path"], a.alongside, svc.s["field"], a.cap)
            after["during"] = False
            reports.append(after)
            if after["status"] == 200 or time.time() - answered > a.recover:
                break
            time.sleep(1)
        res["first_200_after_answer_s"] = round(time.time() - answered, 2) if after["status"] == 200 else None
        stop.set()
        res["alongside_requests"] = reports
        res["state_after"] = svc.state()
        res["processes_after"] = svc.processes()
        res["samples_max_current"] = max((s[1] or 0 for s in samples), default=None)
        res["samples_max_anon"] = max((s[2] or 0 for s in samples), default=None)
        with open(os.path.join(out, "samples.tsv"), "w") as f:
            f.write("t\tcurrent\tanon\n" + "".join(f"{t}\t{c}\t{n}\n" for t, c, n in samples))
    finally:
        svc.remove(out)
        with open(os.path.join(out, "result.json"), "w") as f:
            json.dump(res, f, indent=1)
    h = res.get("hostile", {})
    side = [r["status"] for r in res.get("alongside_requests", [])]
    print(
        f"{a.subject} {res['doc']}: {h.get('status')} in {h.get('seconds')} s; peak "
        f"{(res.get('state_after') or {}).get('memory_peak')}; alongside {side}; after "
        f"{ {k: res.get('state_after', {}).get(k) for k in ('running', 'oom_killed', 'ready')} }; "
        f"first 200 after the "
        f"answer: {res.get('first_200_after_answer_s')} s"
    )


def one(a):
    os.makedirs(a.out, exist_ok=True)
    with open(os.path.join(a.out, "results.jsonl"), "a") as log:
        for doc in a.docs:
            svc = Service(a.subject, f"docx-to-pdf-torture-{a.subject}-one", 18125)
            try:
                ready = svc.wait_ready()
                rec, pdf = post(svc.url + svc.s["path"], doc, svc.s["field"], a.cap)
                rec["ready_s"] = ready
                rec["state"] = svc.state()
                if rec["status"] == 200:
                    p = os.path.join(a.out, os.path.basename(doc) + ".pdf")
                    open(p, "wb").write(pdf)
                    rec["pdfimages"] = sh("pdfimages", "-list", p, check=False).splitlines()[2:]
                    os.remove(p)
            finally:
                svc.remove()
            print(json.dumps(rec), flush=True)
            log.write(json.dumps(rec) + "\n")


def raw_request(host, port, path, head, body, pieces, pause, stall_at=None, cap=200):
    """Send a POST whose body comes in `pieces` with `pause` seconds between them; with stall_at, stop sending at that
    byte and wait. Returns (status line or what happened, seconds)."""
    s = socket.create_connection((host, port), timeout=cap)
    t0 = time.time()
    s.sendall(head)
    try:
        if stall_at is not None:
            s.sendall(body[:stall_at])
        else:
            n = len(body)
            cuts = [n * i // pieces for i in range(pieces + 1)]
            for i in range(pieces):
                s.sendall(body[cuts[i] : cuts[i + 1]])
                if i < pieces - 1:
                    time.sleep(pause)
        data = b""
        while b"\r\n" not in data:
            chunk = s.recv(4096)
            if not chunk:
                return f"closed after {len(data)} bytes", round(time.time() - t0, 2)
            data += chunk
        status = data.split(b"\r\n", 1)[0].decode("latin-1")
        rest = data
        try:
            s.settimeout(30)
            while True:
                c = s.recv(65536)
                if not c:
                    break
                rest += c
        except OSError:
            pass
        return status + f" ({len(rest)} bytes)", round(time.time() - t0, 2)
    except OSError as e:
        return f"{type(e).__name__}: {e}", round(time.time() - t0, 2)
    finally:
        s.close()


def pause(a):
    os.makedirs(a.out, exist_ok=True)
    svc = Service(a.subject, f"docx-to-pdf-torture-{a.subject}-pause", 18126)
    res = {"subject": a.subject, "doc": os.path.basename(a.doc)}
    try:
        res["ready_s"] = svc.wait_ready()
        body, ctype = body_of(a.doc, svc.s["field"])
        head = (
            f"POST {svc.s['path']} HTTP/1.1\r\nHost: localhost\r\nContent-Type: {ctype}\r\n"
            f"Content-Length: {len(body)}\r\nConnection: close\r\n\r\n"
        ).encode()
        res["pauses"] = []
        for p in [float(x) for x in a.pauses.split(",")]:
            st, sec = raw_request("127.0.0.1", svc.port, svc.s["path"], head, body, 4, p, cap=a.stall)
            res["pauses"].append({"pause_s": p, "pieces": 4, "answer": st, "seconds": sec})
            print(a.subject, "pause", p, st, sec, flush=True)
        st, sec = raw_request(
            "127.0.0.1", svc.port, svc.s["path"], head, body, 1, 0, stall_at=len(body) // 2, cap=a.stall
        )
        res["stall"] = {"answer": st, "seconds": sec, "cap": a.stall}
        print(a.subject, "stall", st, sec, flush=True)
        res["state_after"] = svc.state()
    finally:
        svc.remove(a.out)
        with open(os.path.join(a.out, "result.json"), "w") as f:
            json.dump(res, f, indent=1)


def egress(a):
    """image/egress-test.sh's set-up for any subject: the listener and the service share a private network
    (10.231.N.0/24) and a stand-in internet (198.18.N.0/24), both internal; a client on the private one posts every
    document. The listener's log says whether anything connected."""
    out = os.path.abspath(a.out)
    os.makedirs(f"{out}/pdf", exist_ok=True)
    os.chmod(f"{out}/pdf", 0o777)
    n = 124
    eg = os.path.abspath(a.egress)
    tag = f"docx-to-pdf-torture-eg-{a.subject}"
    priv, pub = f"{tag}-private", f"{tag}-public"
    for c in (f"{tag}-listener", f"{tag}-client", f"{tag}-service"):
        sh("docker", "rm", "-f", "-v", c, check=False)
    sh("docker", "network", "rm", priv, pub, check=False)
    sh("docker", "network", "create", "--internal", "--subnet", f"10.231.{n}.0/24", priv)
    sh("docker", "network", "create", "--internal", "--subnet", f"198.18.{n}.0/24", pub)
    docs = f"{out}/docs"
    sh("rm", "-rf", docs)
    sh("python3", f"{eg}/documents.py", docs, f"198.18.{n}.10", f"10.231.{n}.10")
    svc = None
    try:
        sh(
            "docker",
            "run",
            "-d",
            "--name",
            f"{tag}-listener",
            "--network",
            priv,
            "--ip",
            f"10.231.{n}.10",
            "-v",
            f"{eg}:/egress:ro",
            PYTHON,
            "python",
            "-u",
            "/egress/listener.py",
            "80",
            "81",
            "443",
            "21",
            "445",
        )
        sh("docker", "network", "connect", "--ip", f"198.18.{n}.10", pub, f"{tag}-listener")
        svc = Service(a.subject, f"{tag}-service", 0, network=priv)
        sh("docker", "network", "connect", "--ip", f"198.18.{n}.20", pub, svc.name)
        nets = json.loads(sh("docker", "inspect", "-f", "{{json .NetworkSettings.Networks}}", svc.name))
        ip = nets[priv]["IPAddress"]
        base = f"http://{ip}:{svc.s['port']}"
        client = [
            "docker",
            "run",
            "--rm",
            "--name",
            f"{tag}-client",
            "--network",
            priv,
            "-v",
            f"{HERE}/torture.py:/t.py:ro",
            "-v",
            f"{docs}:/docs:ro",
            "-v",
            f"{out}/pdf:/out",
            PYTHON,
        ]
        wait = subprocess.run(
            client
            + [
                "python",
                "-c",
                f"""
import time, urllib.request
for _ in range(300):
    try:
        urllib.request.urlopen('{base}{svc.s["ready"]}', timeout=2); break
    except Exception:
        time.sleep(1)
else:
    raise SystemExit(1)""",
            ],
            capture_output=True,
            text=True,
        )
        if wait.returncode:
            raise RuntimeError("the service never became ready")
        before = sh("docker", "logs", f"{tag}-listener", check=False).count('"connect"')
        r = subprocess.run(
            client
            + ["python", "-u", "/t.py", "post", base + svc.s["path"], svc.s["field"], "/docs", "/out", str(a.cap)],
            capture_output=True,
            text=True,
        )
        open(f"{out}/client.log", "w").write(r.stdout + r.stderr)
        lst = subprocess.run(["docker", "logs", f"{tag}-listener"], capture_output=True, text=True)
        open(f"{out}/listener.jsonl", "w").write(lst.stdout + lst.stderr)
        open(f"{out}/state.json", "w").write(json.dumps(svc.state()))
        print(
            f"{a.subject}: listener connections before {before}, after "
            f"{(lst.stdout + lst.stderr).count(chr(34) + 'connect' + chr(34))}"
        )
    finally:
        if svc:
            svc.remove(out)
        for c in (f"{tag}-listener", f"{tag}-client"):
            sh("docker", "rm", "-f", "-v", c, check=False)
        sh("docker", "network", "rm", priv, pub, check=False)


def post_all(a):
    names = sorted(os.listdir(a.docs))
    with open(os.path.join(a.out, "results.jsonl"), "a") as log:
        for nm in names:
            rec, pdf = post(a.url, os.path.join(a.docs, nm), a.field, a.cap)
            if rec["status"] == 200:
                open(os.path.join(a.out, nm + ".pdf"), "wb").write(pdf)
            print(json.dumps(rec), flush=True)
            log.write(json.dumps(rec) + "\n")


def soak(a):
    os.makedirs(a.out, exist_ok=True)
    rows = []
    with open(a.manifest) as f:
        head = f.readline().rstrip("\n").split("\t")
        for line in f:
            r = dict(zip(head, line.rstrip("\n").split("\t"), strict=False))
            if r["class"] in a.classes.split(","):
                rows.append(r)
    docs = [os.path.join(a.doc_dir, r["path"]) for r in rows]
    svc = Service(a.subject, f"docx-to-pdf-torture-soak-{a.name}", a.port, cpus=a.cpus, memory=a.memory, env=a.env)
    meta = {
        "subject": a.subject,
        "image": svc.s["image"],
        "env": svc.env,
        "cpus": a.cpus,
        "n": a.n,
        "docs": [os.path.basename(d) for d in docs],
        "began": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    tsv = open(os.path.join(a.out, "soak.tsv"), "w")
    tsv.write("i\tdoc\tstatus\tseconds\tcurrent\tanon\trss_kib\tt\n")
    try:
        meta["ready_s"] = svc.wait_ready()
        for i in range(a.n):
            d = docs[i % len(docs)]
            rec, _ = post(svc.url + svc.s["path"], d, svc.s["field"], 600)
            cur, anon = svc.memory()
            tsv.write(
                f"{i}\t{rec['doc']}\t{rec['status']}\t{rec['seconds']}\t{cur}\t{anon}\t"
                f"{json.dumps(svc.processes(), separators=(',', ':'))}\t{rec['t0']}\n"
            )
            tsv.flush()
        meta["state_after"] = svc.state()
    finally:
        meta["ended"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        svc.remove(a.out)
        json.dump(meta, open(os.path.join(a.out, "soak.json"), "w"), indent=1)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    h = sub.add_parser("hostile")
    h.add_argument("subject", choices=SUBJECTS)
    h.add_argument("doc")
    h.add_argument("--alongside", required=True)
    h.add_argument("--cap", type=int, default=300)
    h.add_argument(
        "--recover",
        type=int,
        default=0,
        help="after the hostile answer, resend the report until a 200 or this many seconds (0: once)",
    )
    h.add_argument("--out", required=True)
    o = sub.add_parser("one")
    o.add_argument("subject", choices=SUBJECTS)
    o.add_argument("docs", nargs="+")
    o.add_argument("--cap", type=int, default=300)
    o.add_argument("--out", required=True)
    q = sub.add_parser("pause")
    q.add_argument("subject", choices=SUBJECTS)
    q.add_argument("doc")
    q.add_argument("--pauses", default="2,2,2")
    q.add_argument("--stall", type=int, default=200)
    q.add_argument("--out", required=True)
    e = sub.add_parser("egress")
    e.add_argument("subject", choices=SUBJECTS)
    e.add_argument("egress", help="image/egress (documents.py, listener.py)")
    e.add_argument("--cap", type=int, default=300)
    e.add_argument("--out", required=True)
    s = sub.add_parser("soak")
    s.add_argument("subject", choices=SUBJECTS)
    s.add_argument("doc_dir")
    s.add_argument("--manifest", required=True)
    s.add_argument("--classes", default="report,objects")
    s.add_argument("-n", type=int, default=3000)
    s.add_argument("--name", required=True)
    s.add_argument("--port", type=int, required=True)
    s.add_argument("--cpus", default="0")
    s.add_argument("--memory", default="4g")
    s.add_argument("--env", action="append", default=[])
    s.add_argument("--out", required=True)
    c = sub.add_parser("post")
    c.add_argument("url")
    c.add_argument("field")
    c.add_argument("docs")
    c.add_argument("out")
    c.add_argument("cap", type=int)
    a = p.parse_args()
    {"hostile": hostile, "one": one, "pause": pause, "egress": egress, "soak": soak, "post": post_all}[a.cmd](a)


if __name__ == "__main__":
    sys.exit(main())
