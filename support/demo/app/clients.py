"""Zero-dependency stdlib clients for PostgreSQL 18, Redis 7, and S3 (AWS SigV4)."""

from __future__ import annotations

import datetime as dt
import hashlib
import hmac
import os
import socket
import struct
import urllib.error
import urllib.parse
import urllib.request


def _recv_exact(sock: socket.socket, n: int) -> bytes:
    buf = bytearray()
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise ConnectionError("unexpected EOF from postgres")
        buf.extend(chunk)
    return bytes(buf)


def pg_query(sql: str, dsn: str | None = None) -> list[dict[str, str | None]]:
    """Executes a simple SQL query over PostgreSQL v3 wire protocol (trust auth)."""
    parsed = urllib.parse.urlparse(dsn or os.environ["DATABASE_URL"])
    host = parsed.hostname or "127.0.0.1"
    port = parsed.port or 5432
    user = urllib.parse.unquote(parsed.username or "postgres")
    database = (parsed.path or "/app_db").lstrip("/") or "app_db"

    with socket.create_connection((host, port), timeout=5.0) as sock:
        params = (
            b"user\x00" + user.encode() + b"\x00"
            b"database\x00" + database.encode() + b"\x00"
            b"client_encoding\x00UTF8\x00\x00"
        )
        startup = struct.pack("!II", 8 + len(params), 196608) + params
        sock.sendall(startup)

        while True:
            tag = _recv_exact(sock, 1)
            (length,) = struct.unpack("!I", _recv_exact(sock, 4))
            body = _recv_exact(sock, length - 4)
            if tag == b"R":
                (auth_code,) = struct.unpack("!I", body[:4])
                if auth_code != 0:
                    raise RuntimeError(f"unsupported pg auth mode {auth_code}")
            elif tag == b"E":
                raise RuntimeError(
                    f"pg startup error: {body.decode('utf-8', 'replace')}"
                )
            elif tag == b"Z":
                break

        payload = sql.encode("utf-8") + b"\x00"
        sock.sendall(b"Q" + struct.pack("!I", 4 + len(payload)) + payload)

        columns: list[str] = []
        rows: list[dict[str, str | None]] = []
        error_msg: str | None = None

        while True:
            tag = _recv_exact(sock, 1)
            (length,) = struct.unpack("!I", _recv_exact(sock, 4))
            body = _recv_exact(sock, length - 4)
            if tag == b"T":
                (num_fields,) = struct.unpack("!H", body[:2])
                pos = 2
                columns = []
                for _ in range(num_fields):
                    end = body.index(b"\x00", pos)
                    columns.append(body[pos:end].decode("utf-8"))
                    pos = end + 1 + 18
            elif tag == b"D":
                (num_cols,) = struct.unpack("!H", body[:2])
                pos = 2
                row: dict[str, str | None] = {}
                for idx in range(num_cols):
                    (col_len,) = struct.unpack("!i", body[pos : pos + 4])
                    pos += 4
                    col_name = columns[idx] if idx < len(columns) else f"col_{idx}"
                    if col_len == -1:
                        row[col_name] = None
                    else:
                        row[col_name] = body[pos : pos + col_len].decode("utf-8")
                        pos += col_len
                rows.append(row)
            elif tag == b"E":
                error_msg = body.decode("utf-8", "replace")
            elif tag == b"Z":
                break

        if error_msg is not None:
            raise RuntimeError(f"pg query failed: {error_msg}")
        return rows


def redis_cmd(*args: str, url: str | None = None) -> str | int | None:
    """Executes one Redis RESP2 command."""
    parsed = urllib.parse.urlparse(url or os.environ["REDIS_URL"])
    host = parsed.hostname or "127.0.0.1"
    port = parsed.port or 6379
    encoded = [f"*{len(args)}\r\n".encode()]
    for arg in args:
        raw = str(arg).encode("utf-8")
        encoded.append(f"${len(raw)}\r\n".encode() + raw + b"\r\n")
    with socket.create_connection((host, port), timeout=3.0) as sock:
        sock.sendall(b"".join(encoded))
        f = sock.makefile("rb")
        line = f.readline()
        if not line:
            return None
        prefix, payload = line[:1], line[1:].rstrip(b"\r\n")
        if prefix == b"+":
            return payload.decode("utf-8")
        if prefix == b":":
            return int(payload)
        if prefix == b"$":
            size = int(payload)
            if size < 0:
                return None
            data = f.read(size)
            f.read(2)
            return data.decode("utf-8")
        if prefix == b"-":
            raise RuntimeError(f"redis error: {payload.decode('utf-8', 'replace')}")
        return payload.decode("utf-8", "replace")


def _sign(key: bytes, msg: str) -> bytes:
    return hmac.new(key, msg.encode("utf-8"), hashlib.sha256).digest()


def s3_request(
    method: str,
    bucket: str,
    key: str,
    body: bytes = b"",
    content_type: str = "application/octet-stream",
    signed: bool = True,
) -> tuple[int, bytes]:
    """Performs an S3 request (AWS SigV4 or anonymous) against the native S3 server."""
    endpoint = os.environ.get("S3_ENDPOINT", "http://127.0.0.1:8333").rstrip("/")
    parsed = urllib.parse.urlparse(endpoint)
    host = parsed.netloc
    uri = f"/{bucket}/{key.lstrip('/')}"
    url = f"{endpoint}{uri}"

    headers: dict[str, str] = {"Host": host}
    if signed:
        access_key = os.environ["S3_ACCESS_KEY"]
        secret_key = os.environ["S3_SECRET_KEY"]
        region = "us-east-1"
        service = "s3"
        now = dt.datetime.now(dt.UTC)
        amz_date = now.strftime("%Y%m%dT%H%M%SZ")
        date_stamp = now.strftime("%Y%m%d")
        payload_hash = hashlib.sha256(body).hexdigest()

        headers["x-amz-date"] = amz_date
        headers["x-amz-content-sha256"] = payload_hash
        if body:
            headers["Content-Type"] = content_type

        signed_names = sorted(k.lower() for k in headers)
        canonical_headers = "".join(
            f"{k.lower()}:{headers[orig].strip()}\n"
            for k in signed_names
            for orig in headers
            if orig.lower() == k
        )
        signed_headers = ";".join(signed_names)
        canonical_request = "\n".join(
            [method, uri, "", canonical_headers, signed_headers, payload_hash]
        )
        credential_scope = f"{date_stamp}/{region}/{service}/aws4_request"
        string_to_sign = "\n".join(
            [
                "AWS4-HMAC-SHA256",
                amz_date,
                credential_scope,
                hashlib.sha256(canonical_request.encode("utf-8")).hexdigest(),
            ]
        )
        k_date = _sign(("AWS4" + secret_key).encode("utf-8"), date_stamp)
        k_region = _sign(k_date, region)
        k_service = _sign(k_region, service)
        k_signing = _sign(k_service, "aws4_request")
        signature = hmac.new(
            k_signing, string_to_sign.encode("utf-8"), hashlib.sha256
        ).hexdigest()
        headers["Authorization"] = (
            f"AWS4-HMAC-SHA256 Credential={access_key}/{credential_scope}, "
            f"SignedHeaders={signed_headers}, Signature={signature}"
        )

    req = urllib.request.Request(
        url, data=body if method == "PUT" else None, method=method
    )
    for k, v in headers.items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=5.0) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as err:
        return err.code, err.read()
