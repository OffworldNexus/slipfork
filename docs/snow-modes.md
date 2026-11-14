# `snow.yml` Modes & Secrets

Slipfork shares the [`snow.yml`](https://delos-2.gitbook.io/kerfufoo/reference/snow-file) manifest with **Kerfufoo** so your cloud deployment and your local/test worlds never drift apart.

---

## `modes` (`all`, `cloud`, `local`)

Real applications share 80% of their wiring (`DATABASE_URL`, `REDIS_URL`, `API_URL`) across every environment while differing on the remaining 20% (cloud object storage & Sentry vs local SeaweedFS & `DEBUG`).

`snow.yml` models this with top-level `modes`:

```yaml title="snow.yml"
modes:
  all:
    envs:
      django:
        SECRET_KEY: '${{ secrets::SECRET_KEY !! random_string(50) }}'
        DATABASE_URL: '${{ resources::pg_databases.psql.url }}'
        REDIS_URL: '${{ resources::redis_clusters.redis.url }}'

  cloud:
    envs:
      django:
        STORAGES_MODE: 'do'
        AWS_STORAGE_BUCKET_NAME: '${{ vars::AWS_STORAGE_BUCKET_NAME }}'
        AWS_SECRET_ACCESS_KEY: '${{ secrets::AWS_SECRET_ACCESS_KEY }}'

  local:
    envs:
      django:
        DEBUG: 'True'
        STORAGES_MODE: 's3'
        AWS_S3_ENDPOINT_URL: '${{ resources::s3_buckets.media.endpoint_url }}'
```

### Resolution Rules

1. **`modes` and top-level `envs` are mutually exclusive.** Defining both in the same `snow.yml` is a validation error.
2. **`all` is the reserved universal mode.** For an active mode `<M>`, each env group `<G>` resolves by merging `modes.all.envs.<G>` first, then overlaying `modes.<M>.envs.<G>`.
3. **Default mode selection:**
   - **Slipfork (`slipfork up` / `slipfork test`):** defaults to `local` (override with `--mode <name>`).
   - **Kerfufoo (`EnvGroup.mode`):** defaults to `cloud`; any `EnvGroup` in Kerfufoo can select a custom mode defined in `modes:` (for example `staging` or `sandbox`).

---

## Auto-Generated Secrets (`!!`)

Some secrets (`SECRET_KEY`, internal HMAC tokens, webhook signing keys) do not come from a third-party vendor — they just need to be cryptographically random and stable across restarts.

Use the **`!!` generate-and-persist operator** on `secrets::<KEY>`:

```yaml
modes:
  all:
    envs:
      django:
        SECRET_KEY: '${{ secrets::SECRET_KEY !! random_string(50) }}'
        INTERNAL_HMAC: '${{ secrets::INTERNAL_HMAC !! random_hex(32) }}'
        INSTANCE_UUID: '${{ secrets::INSTANCE_UUID !! uuid4() }}'
```

### `??` vs `!!`

| Operator | Syntax | Behavior when key is missing | Allowed namespaces |
|---|---|---|---|
| **Fallback (`??`)** | `${{ vars::LOG_LEVEL ?? "INFO" }}` | Returns `"INFO"` transiently; **never writes** to any store | Any (`vars`, `secrets`, `domains`) |
| **Ensure (`!!`)** | `${{ secrets::KEY !! random_string(50) }}` | Generates once via CSPRNG, **persists atomically** (`put_if_absent`) into the active secret vault, and returns it on every subsequent run | `secrets` only |

Built-in generators:

- `random_string(N)` — `N` URL/env-safe alphanumeric characters (`A-Za-z0-9`, `1 <= N <= 512`).
- `random_hex(N)` — `N` random bytes encoded as `2N` lowercase hex chars (`1 <= N <= 256`).
- `random_base64(N)` — `N` random bytes encoded as URL-safe base64 (`1 <= N <= 256`).
- `uuid4()` — RFC 4122 v4 random UUID.

---

## Missing Required Values Fail Closed

If an expression like `${{ secrets::STRIPE_API_KEY }}` has neither `??` nor `!!` and is absent from the active secret provider, Slipfork **refuses to boot** and prints **every** missing key in one shot:

```text
slipfork: 2 required values missing for mode 'local':
  - secrets::STRIPE_API_KEY   (referenced by modes.local.envs.django.STRIPE_API_KEY)
  - vars::PUBLIC_MAP_STYLE    (referenced by modes.all.envs.front.PUBLIC_MAP_STYLE)
Fix: run `slipfork secret set STRIPE_API_KEY` or connect a remote provider (`slipfork auth login`).
```

---

## Open Secret Provider Protocol (`OSPP v1`)

Slipfork is not hardwired to a single cloud control plane. Any secret backend — **Kerfufoo**, Vault, 1Password, or team tooling — can serve variables, secrets, and domains over the open JSON protocol:

```yaml title="snow.yml (optional provider hint) or ~/.config/slipfork/providers.toml"
# Connect local mode to a Kerfufoo / OSPP v1 endpoint for shared team vars/secrets:
# $ slipfork auth login https://kerfufoo.example.com --project acme-shop
```

- **Reads (`vars::*`, `secrets::*`, `domains::*`):** Local vault overrides -> Remote OSPP v1 bundle (`GET /v1/projects/{slug}/modes/{mode}/bundle`).
- **`!!` Generations in `local` mode:** Saved into your local `0600` vault (`~/.local/share/slipfork/vaults/<project>.json`) so `slipfork up` works offline and never mutates shared cloud secrets. In `cloud` mode, Kerfufoo persists `!!` secrets directly into the `EnvGroup` store.
