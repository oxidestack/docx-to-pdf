"""Logs every TCP connection as a JSON line on stdout. Port 80 answers HTTP with content
that marks the request (a PNG, a DOCX, plain text, a DTD); every other port accepts, reads, and never answers (a
host that hangs). Usage: listener.py PORT..."""

import asyncio
import io
import json
import struct
import sys
import time
import zipfile
import zlib


def png(rgb=(255, 0, 255), w=64, h=64):
    raw = b"".join(b"\x00" + bytes(rgb) * w for _ in range(h))

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw))
        + chunk(b"IEND", b"")
    )


def docx(text):
    b = io.BytesIO()
    with zipfile.ZipFile(b, "w") as z:
        z.writestr(
            "[Content_Types].xml",
            '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>',
        )
        z.writestr(
            "_rels/.rels",
            '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>',
        )
        z.writestr(
            "word/document.xml",
            f'<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>',
        )
    return b.getvalue()


def log(**kw):
    print(json.dumps({"t": round(time.time(), 3), **kw}), flush=True)


async def handle(reader, writer, port):
    peer = writer.get_extra_info("peername")
    local = writer.get_extra_info("sockname")
    try:
        first = await asyncio.wait_for(reader.read(2048), 10)
    except Exception:
        first = b""
    line = first.split(b"\r\n", 1)[0].decode("latin-1")
    log(event="connect", local=f"{local[0]}:{local[1]}", peer=peer[0], first=line[:200])
    if port != 80:
        await asyncio.sleep(600)  # the tarpit
        writer.close()
        return
    path = line.split(" ")[1] if line.count(" ") >= 2 else "/"
    marker = "EGRESSMARKER" + path.replace("/", "-").replace(".", "-")
    if path.endswith((".png", ".jpg", ".gif", ".emf", ".svg")):
        body, ctype = png(), "image/png"
    elif path.endswith((".docx", ".dotx", ".docm")):
        body, ctype = docx(marker), "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    elif path.endswith(".dtd"):
        body, ctype = f'<!ENTITY ext "{marker}">'.encode(), "application/xml-dtd"
    else:
        body, ctype = (marker + "\n").encode(), "text/plain"
    writer.write(
        f"HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {len(body)}\r\nConnection: close\r\n\r\n".encode()
        + body
    )
    await writer.drain()
    writer.close()


async def main():
    ports = [int(p) for p in sys.argv[1:]] or [80, 81, 443, 21, 445]
    for p in ports:
        await asyncio.start_server(lambda r, w, p=p: handle(r, w, p), "0.0.0.0", p)
    log(event="listening", ports=ports)
    await asyncio.Event().wait()


asyncio.run(main())
