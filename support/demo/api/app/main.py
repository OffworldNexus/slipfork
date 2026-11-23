"""FastAPI backend: world status, item CRUD, SMTP test and S3 upload over real services."""

from __future__ import annotations

import os
import pathlib
import uuid

import redis
from fastapi import FastAPI, HTTPException, Response, UploadFile
from pydantic import BaseModel

from app import db, mail, s3
from app.world import fork_name, public_host, public_url, world_id

app = FastAPI(title="Slipfork demo API", version="0.1.0")

ASSETS_BUCKET = "assets"
UPLOADS_BUCKET = "uploads"
PUBLIC_ASSET = "badge.svg"


def asset_url() -> str:
    return f"http://assets.{public_host()}/{PUBLIC_ASSET}"


def mail_url() -> str:
    return f"http://mail.{public_host()}/"


def uploads_url() -> str:
    return os.environ.get("UPLOADS_URL", f"http://uploads.{public_host()}").rstrip("/")


def git_branch() -> str:
    head = pathlib.Path("/project/.git/HEAD")
    if not head.is_file():
        return "unknown"
    raw = head.read_text().strip()
    if raw.startswith("ref: refs/heads/"):
        return raw.removeprefix("ref: refs/heads/")
    return raw[:12]


def redis_client() -> redis.Redis:
    return redis.Redis.from_url(os.environ["REDIS_URL"], decode_responses=True)


class ItemCreate(BaseModel):
    title: str


@app.get("/api/healthz")
def healthz() -> dict[str, bool]:
    return {"ok": True}


@app.get("/api/status")
def status() -> dict[str, object]:
    health = {"postgres": False, "redis": False, "s3": False}
    items = 0
    events = 0
    activity = 0
    try:
        items = int(db.query("SELECT count(*) AS n FROM items")[0]["n"])
        events = int(db.query("SELECT count(*) AS n FROM events")[0]["n"])
        health["postgres"] = True
    except Exception:
        pass
    try:
        client = redis_client()
        activity = int(client.get("activity") or 0)
        client.ping()
        health["redis"] = True
    except Exception:
        pass
    try:
        health["s3"] = s3.exists(ASSETS_BUCKET, PUBLIC_ASSET)
    except Exception:
        pass
    return {
        "world": world_id(),
        "fork": fork_name(),
        "branch": git_branch(),
        "public_url": public_url(),
        "asset_url": asset_url(),
        "mail_url": mail_url(),
        "health": health,
        "counts": {"items": items, "events": events},
        "redis": {"activity": activity},
    }


@app.get("/api/items")
def list_items() -> list[dict[str, object]]:
    return db.query(
        "SELECT id, title, done, created_at::text AS created_at FROM items ORDER BY id"
    )


@app.post("/api/items", status_code=201)
def create_item(payload: ItemCreate) -> dict[str, object]:
    title = payload.title.strip()[:120]
    if not title:
        raise HTTPException(status_code=400, detail="title is required")
    rows = db.execute(
        "INSERT INTO items (title) VALUES (%s) "
        "RETURNING id, title, done, created_at::text AS created_at",
        (title,),
    )
    return rows[0]


@app.patch("/api/items/{item_id}")
def toggle_item(item_id: int) -> dict[str, object]:
    rows = db.execute(
        "UPDATE items SET done = NOT done WHERE id = %s "
        "RETURNING id, title, done, created_at::text AS created_at",
        (item_id,),
    )
    if not rows:
        raise HTTPException(status_code=404, detail=f"item {item_id} not found")
    return rows[0]


@app.delete("/api/items/{item_id}")
def delete_item(item_id: int) -> dict[str, object]:
    rows = db.execute("DELETE FROM items WHERE id = %s RETURNING id", (item_id,))
    if not rows:
        raise HTTPException(status_code=404, detail=f"item {item_id} not found")
    return {"deleted": rows[0]["id"]}


@app.post("/api/test-email")
def test_email() -> dict[str, object]:
    try:
        delivered = mail.send_test(world_id(), public_url())
    except mail.MailDisabled as exc:
        raise HTTPException(status_code=503, detail=str(exc)) from exc
    except OSError as exc:
        raise HTTPException(
            status_code=502, detail=f"SMTP delivery failed: {exc}"
        ) from exc
    return {"delivered": delivered, "world": world_id(), "inbox": mail_url()}


@app.post("/api/upload")
async def upload(file: UploadFile) -> dict[str, object]:
    body = await file.read()
    key = f"{world_id()}-{uuid.uuid4().hex[:8]}-{file.filename or 'blob'}"
    s3.put(UPLOADS_BUCKET, key, body, file.content_type or "application/octet-stream")
    return {
        "bucket": UPLOADS_BUCKET,
        "key": key,
        "bytes": len(body),
        "private_url": f"{uploads_url()}/{key}",
        "preview": f"/api/uploads/{key}",
    }


@app.get("/api/uploads/{key:path}")
def preview_upload(key: str) -> Response:
    """Serve a private object through the app so the uploads bucket stays 403 anonymous."""
    try:
        body, content_type = s3.fetch(UPLOADS_BUCKET, key)
    except Exception as exc:
        raise HTTPException(status_code=404, detail=f"upload {key} not found") from exc
    return Response(content=body, media_type=content_type)


@app.get("/api/assets")
def assets() -> dict[str, object]:
    return {
        "bucket": ASSETS_BUCKET,
        "keys": s3.keys(ASSETS_BUCKET),
        "preview": asset_url(),
    }
