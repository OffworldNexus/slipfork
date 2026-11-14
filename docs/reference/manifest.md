# Manifest & Secret Protocol Reference

## 1. `snow.yml` v2 Schema Extensions

Slipfork supports the full [Snow file specification](https://delos-2.gitbook.io/kerfufoo/reference/snow-file) (`images`, `components`, `ingresses`, `hooks`, `domains`, `resources`) with three additions:

### `modes` (replaces top-level `envs`)

```yaml
modes:
  all:
    envs:
      <group_id>:
        <VAR_NAME>: '<faceless_template>'
  <mode_name>:   # e.g. cloud, local, staging
    envs:
      <group_id>:
        <VAR_NAME>: '<faceless_template>'
```

- **Mutual exclusion:** A `snow.yml` file MUST NOT declare both top-level `envs` and top-level `modes`.
- **Merge precedence:** For active mode `M` and component `envs: [g1, g2]`:
  1. For each group `g`, compute `effective_group(g, M) = merge(modes.all.envs[g], modes[M].envs[g])`.
  2. Merge `[effective_group(g1, M), effective_group(g2, M)]` left-to-right (later group wins on key collision, matching Snow v1).
  3. Build-time vs run-time conflict validation runs on the resolved `effective_group`s for mode `M`.

### `resources.s3_buckets`

```yaml
resources:
  s3_buckets:
    - id: media
```

Exposes `resources::s3_buckets.<id>.{endpoint_url, bucket, access_key, secret_key, region}` (backed by SeaweedFS in `local` mode and DigitalOcean Spaces / S3 in `cloud` mode).

### `!!` Secret Generator Expressions

Inside `${{ ... }}`:

```text
expr       := namespace "::" path ( ("??" json_string) | ("!!" generator) )?
generator  := "random_string(" int ")"
            | "random_hex(" int ")"
            | "random_base64(" int ")"
            | "uuid4()"
```

- `!!` is valid **only** when `namespace == "secrets"` and `path` is a single key (`secrets::<KEY>`). Using `!!` on `vars::`, `resources::`, `components::`, `domains::`, or `deployment::` is a schema validation error.

---

## 2. Open Secret Provider Protocol (`OSPP v1`)

Any remote control plane (including **Kerfufoo**) implements two authenticated HTTPS JSON endpoints to supply `vars`, `secrets`, and `domains` to Slipfork or CI runners:

### `GET /v1/projects/{project}/modes/{mode}/bundle`

- **Headers:** `Authorization: Bearer <token>`, `Accept: application/json`
- **Response `200 OK`:**
  ```json
  {
    "project": "acme-shop",
    "mode": "local",
    "vars": { "PUBLIC_MAP_STYLE": "positron" },
    "secrets": { "STRIPE_API_KEY": "sk_test_..." },
    "domains": { "main": "acme.localtest.me" }
  }
  ```

### `POST /v1/projects/{project}/modes/{mode}/secrets:ensure`

Atomically persists an auto-generated `!!` secret if and only if `key` does not yet exist (`put_if_absent`), returning the authoritative value so concurrent callers never race:

- **Request:**
  ```json
  { "key": "SECRET_KEY", "proposed_value": "9fK2..." }
  ```
- **Response `200 OK`:**
  ```json
  { "key": "SECRET_KEY", "value": "9fK2...", "created": true }
  ```
