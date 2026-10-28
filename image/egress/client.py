"""POSTs every document in /docs to URL, one at a time; writes /out/<name>.pdf and a JSON line per
document to /out/results.jsonl. Usage: client.py URL [PREFIX,...]"""

import json
import os
import sys
import time
import urllib.error
import urllib.request
import uuid

URL = sys.argv[1]
names = sorted(os.listdir("/docs"))
if len(sys.argv) > 2:
    names = [n for n in names if any(n.startswith(p) for p in sys.argv[2].split(","))]
with open("/out/results.jsonl", "a") as log:
    for n in names:
        data = open(f"/docs/{n}", "rb").read()
        b = uuid.uuid4().hex
        body = (
            (
                f'--{b}\r\nContent-Disposition: form-data; name="document"; filename="{n}"\r\n'
                "Content-Type: application/octet-stream\r\n\r\n"
            ).encode()
            + data
            + f"\r\n--{b}--\r\n".encode()
        )
        req = urllib.request.Request(URL, body, {"Content-Type": f"multipart/form-data; boundary={b}"})
        t0 = time.time()
        try:
            with urllib.request.urlopen(req, timeout=600) as r:
                status, out = r.status, r.read()
        except urllib.error.HTTPError as e:
            status, out = e.code, e.read()
        except Exception as e:
            status, out = str(e), b""
        t1 = time.time()
        if status == 200:
            open(f"/out/{n}.pdf", "wb").write(out)
        rec = {
            "doc": n,
            "status": status,
            "seconds": round(t1 - t0, 2),
            "t0": round(t0, 3),
            "t1": round(t1, 3),
            "bytes": len(out),
        }
        if status != 200:
            rec["body"] = out[:300].decode("latin-1")
        print(json.dumps(rec), flush=True)
        log.write(json.dumps(rec) + "\n")
