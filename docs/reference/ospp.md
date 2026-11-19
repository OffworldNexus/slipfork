# Secrets & OSPP v1 (`0.1.0`)

Your CI already holds the authoritative values for the real environment. OSPP v1 is how a local world asks for them without copying `.env` files or pasting secrets into a terminal.

Pair once, boot, and `slipforkd` injects the resolved environment into every component and shell it supervises:

```console
$ slipforkd --state-dir ~/.cache/slipfork --runtime-dir ~/.cache/slipfork/run \
    --listen 127.0.0.1:0 --ospp-endpoint wss://ci.acme-shop.example/ospp
$ slipfork auth
https://ci.acme-shop.example/pair#handle=<32-byte hex>
[auth] paired p256:02b1… account=alice
$ slipfork up
$ slipfork exec -- sh -c 'echo ${SECRET_KEY:+injected}'
injected
```

!!! info "`--ospp-endpoint` is a first-run flag"
    Pairing needs a server identity, so the first daemon needs `--ospp-endpoint`. `slipfork auth` then saves `{endpoint, account, machine}` at `<state_dir>/ospp.json` (`0600`), and later auto-spawned or systemd daemons reconnect from that record. An explicit `--ospp-endpoint` still wins when given.

`SECRET_KEY` is an ordinary `modes.<mode>.envs` entry you do **not** claim locally. A claimed key resolves from your vault; an unclaimed one resolves from the provider (`${{ secrets::SECRET_KEY }}`, with no `!!` and no `??`).

## What it buys you

One place for truth: the CI environment owns the variables, secrets, and managed resources, and your world receives them instead of re-declaring them.

Slipfork ships the OSPP v1 **client**. The server belongs to a provider (your CI). Without one, every local world still boots unchanged; OSPP is optional and never a prerequisite.

## Two tiers

The local `0600` vault holds values that are yours and must survive a restart: `secrets::<KEY>` values stored with `slipfork secret set` and the local-mode `!!` values generated once on first resolve. The default path is `~/.local/share/slipfork/vaults/<project>.json`, moved only by `$XDG_DATA_HOME`.

Remote secrets take the other tier. They arrive HPKE-encrypted (RFC 9180) over OSPP, are decrypted inside `slipforkd` memory, and are injected directly into supervised children. Restarting the daemon drops them until the next subscribe; no vault entry or file ever holds one.

## Pairing

1. Start a `slipforkd` with `--ospp-endpoint <wss://…>` before the first `up`/`fork`, unless this machine already paired.
2. Run `slipfork auth`. It dials, proves possession of this machine's key, has `slipforkd` open the server-supplied page through `$BROWSER` (else `xdg-open`), and blocks for the verdict.
3. Approve the machine in that page. It shows the key fingerprint and the hostname being enrolled. This step needs a human; an agent must stop and ask.
4. `slipfork auth` prints the server-supplied URL and `[auth] paired …`. Later commands dial with the enrolled key.

`slipfork auth` is a desktop command: a daemon with no `$BROWSER`, `DISPLAY`, or `WAYLAND_DISPLAY` fails pairing closed and prints no URL.

- **Server-supplied URL:** the whole string (`<url>#handle=<handle>`) comes from the server. Slipfork never builds one from the endpoint host, because browser origin and path may differ.
- **Fragment-only, single-use:** the 32-byte handle lives only in the fragment, so access logs and `Referer` headers never record it, and the page strips it on load.
- **In-memory pairing:** a pending pairing lives in server memory for that WebSocket only. Ctrl-C or a restart cancels it, so re-run `slipfork auth`.
- **One server identity:** `slipfork auth <ENDPOINT>` fails closed when the URL differs from the daemon's endpoint.
- **Machine key:** the enrolled P-256 key is `~/.local/share/slipfork/device-key-software.json` (`0600`). No secret value sits beside it.

!!! warning "One daemon per state directory"
    If a daemon already owns `<state_dir>/run/daemon.sock`, a second one refuses with `already served by a running slipforkd; refusing to steal it`. Stop its worlds with `slipfork down` and end that process first.

## What stays local

Before subscribing, `slipforkd` sends the raw `snow.yml` plus `claims`: the `secrets::` keys in the project's vault, the resources it runs locally plus every declared `domains[].id`, and every local-mode `!!` key.

A claimed entry is never resolved by the server. Its expression comes back untouched and is filled from the local vault. That is how `${{ secrets::LOCAL_PASSWORD }}` wins over CI on your machine while no local value leaves it.

Anything not claimed is resolved by the server, or refused there if missing. Server values whose evaluation touched a secret come back as `{ "encrypted": "<base64>" }` and are decrypted in memory.

## Frames

Every frame is one UTF-8 JSON text message with a `"type"`. Field-level JSON, HPKE parameters, and dump framing live in the normative spec: [`docs/design/ospp.md`](https://github.com/OffworldNexus/slipfork/blob/develop/docs/design/ospp.md).

| Frame | Direction | Purpose |
|---|---|---|
| `hello` | client → server | Protocol `version: 1` and supported auth methods |
| `auth_challenge` / `auth_response` / `auth_ok` | ⇄ | P-256 nonce challenge, proof, and encryption methods |
| `pair_hello` / `pair_challenge` / `pair_begin` | ⇄ | First-time enrolment: proof-of-work plus key proof of possession |
| `pair_handle` | server → client | The `url` and single-use `handle` to open |
| `pair_approved` | server → client | The browser approved, or an `error` (rejected, already paired, throttled, bad proof) |
| `subscribe` | client → server | Verbatim Git `remote`, ephemeral HPKE public key, raw `snow.yml`, `claims` |
| `env_resolved` | server → client | Per-component environment for the subscription |
| `env_updated` | server → client | Live push after an upstream change; affected services restart in place |
| `unsubscribe` | client → server | The project's last world stopped |
| `dump_request` / `dump_ready` | client ⇄ server | Short-lived HTTPS URL for one HPKE-sealed resource dump |
| `error` | server → client | Coded refusal; handshake, pairing, and auth failures close the socket |

## When it fails

- `[resources.pg_databases.appdb] local = "remote"`, unpaired: `resource 'pg_database.appdb' requires an authenticated OSPP session in Local mode`. Run `slipfork auth`, or `up --no-dump` for a clean database.
- `local = "prefer_remote"`, unpaired: the world runs the service locally with a `no_remote_session` row, visible under `slipfork up -v`. Never a silent half-remote world.
- `--refresh-dump` on a local strategy is refused earlier: `` `--refresh-dump` cannot be combined with a local strategy for pg_databases 'appdb' ``.
- `slipfork auth` on a daemon with no endpoint and no saved record fails with `this daemon has no OSPP endpoint` and names the flag to start `slipforkd` with.

## Next

- [Local vault & `slipfork secret`](cli.md#slipfork-secret-local-vault) — what the first tier stores and how to manage it.
- [`snow.yml` modes & secrets](../snow-modes.md) — `!!`, `??`, and the vault path.
- [Manifest reference](manifest.md#expressions) — namespaces and resource strategies.
