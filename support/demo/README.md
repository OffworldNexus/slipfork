# Slipfork `0.1.0` Full-Stack Demo

Standing up PostgreSQL, Redis, two S3 buckets, a background worker, and two web services for every branch is slow, and copying config files around still leaves the data behind. Slipfork boots this whole stack from one `Forkfile` and forks the running world — code, dependencies, database rows, cache keys, and objects — with one instant copy-on-write clone.

## Try It Live

```bash
slipfork                                      # Full world + fork status tree
slipfork doctor                               # Host readiness verdict
curl -s http://demo.localhost/api/status | jq # Parent world API (3 rows)
slipfork fork my-experiment                   # One clone, < 1.0s boot
curl -s http://my-experiment--demo.localhost/api/status | jq
curl -i http://static-cdn.demo.localhost/badge.svg   # 200 OK public-read CDN
curl -i http://uploads.demo.localhost/audit.json     # 403 Forbidden private bucket
curl -I http://mail.demo.localhost/                  # 200 OK Mailpit UI
slipfork logs api                             # Per-service colored logs
slipfork env api                              # Resolved runtime URLs + redacted secrets
slipfork drop my-experiment                   # Instant teardown + clone cleanup
```

## Eight Components

| Component | Kind | Port / Domain |
| :--- | :--- | :--- |
| `pg` | PostgreSQL 18 (direct `postgres` exec) | `postgres.url` |
| `cache` | Redis 7 (direct `redis-server`) | `cache.url` |
| `uploads` | native S3 bucket, private | `uploads.url` (`403` anonymous) |
| `assets` | native S3 bucket, `public-read` + CDN | `static-cdn.url` (`200` anonymous) |
| `api` | `backend` image service | `:8000`, `main.url/api` |
| `worker` | `backend` image worker | portless |
| `front` | `frontend` image service, `arg VITE_CDN_URL` | `:5173`, `main.url/` |
| `mailpit` | `from axllent/mailpit:latest` | `:8025`, `mail.*` |

A fork reuses the parent's sealed `image` clones (`cached`, no Dockerfile re-evaluation), snapshots live PG rows, Redis keys, and S3 objects with one Btrfs ioctl, and keeps the parent untouched.
