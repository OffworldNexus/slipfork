"""boto3 S3 client for the demo buckets (path-style, SigV4)."""

from __future__ import annotations

import os
from typing import Any

import boto3
from botocore.config import Config


def client() -> Any:
    return boto3.client(
        "s3",
        endpoint_url=os.environ["S3_ENDPOINT"],
        aws_access_key_id=os.environ["S3_ACCESS_KEY"],
        aws_secret_access_key=os.environ["S3_SECRET_KEY"],
        region_name="us-east-1",
        config=Config(
            signature_version="s3v4",
            s3={"addressing_style": "path"},
            retries={"max_attempts": 3, "mode": "standard"},
        ),
    )


def put(bucket: str, key: str, body: bytes, content_type: str) -> None:
    client().put_object(Bucket=bucket, Key=key, Body=body, ContentType=content_type)


def keys(bucket: str) -> list[str]:
    response = client().list_objects_v2(Bucket=bucket)
    return [obj["Key"] for obj in response.get("Contents", [])]


def fetch(bucket: str, key: str) -> tuple[bytes, str]:
    """Read a private object server-side; `ClientError` when it is missing."""
    obj = client().get_object(Bucket=bucket, Key=key)
    return obj["Body"].read(), obj.get("ContentType", "application/octet-stream")


def exists(bucket: str, key: str) -> bool:
    try:
        client().head_object(Bucket=bucket, Key=key)
    except Exception:
        return False
    return True
