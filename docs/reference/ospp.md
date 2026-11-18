# Secrets & OSPP v1

Your CI already holds the authoritative values for the real environment. The Open Secret Provider Protocol (**OSPP**) v1 is how a local world asks for them without copying `.env` files or pasting secrets into a terminal: pair this machine once, boot, and `slipforkd` injects the resolved environment into every component and shell it supervises.

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

`SECRET_KEY` is an ordinary `modes.<mode>.envs` entry you do **not** claim locally: a claimed key resolves from your vault, an unclaimed one from the provider (`${{ secrets::SECRET_KEY }}`, no `!!`, no `??`).

Pairing needs a server identity, so it needs a `slipforkd` you start with `--ospp-endpoint` (`wss://`, or `ws://` strictly on loopback), and it must stay in the foreground for the whole session. The daemon that `up`/`fork` auto-spawn and the installed `slipforkd.service` carry no endpoint, and `0.1.0` has no `slipfork daemon stop` verb, so start the endpoint-bearing daemon **before** your first `up`. If a daemon already owns `<state_dir>/run/daemon.sock`, a second one refuses with `already served by a running slipforkd; refusing to steal it`: `slipfork down` its worlds and end that process first.

## What it buys you

One place for truth: the CI environment owns the variables, secrets, and managed resources; your world gets them resolved instead of re-declared. Slipfork ships the OSPP v1 **client** — the server end belongs to a provider (your CI). Without one, every local world still boots unchanged; OSPP is optional, never required to boot a world.

## Two tiers

The local `0600` vault (default `~/.local/share/slipfork/vaults/<project>.json`, moved only by `$XDG_DATA_HOME`) holds values that are **yours and must survive a restart**: the `secrets::<KEY>` values you store with `slipfork secret set` (an app secret, or a password for a database you run yourself) and the local-mode `!!` values generated once on first resolve. Remote secrets take the other tier: they arrive HPKE-encrypted (RFC 9180 end-to-end encryption) over OSPP, are decrypted inside `slipforkd` memory, are injected directly into the environment of the supervised children, and live only there — restarting the daemon drops them until the next subscribe, and no vault entry or file on disk ever holds one.

## Pairing

1. Start `slipforkd` with `--ospp-endpoint <wss://…>` before the first `up`/`fork` (above).
2. Run `slipfork auth`. It dials, proves possession of this machine's key, has **`slipforkd`** open the server-supplied page through that host's `$BROWSER` (else `xdg-open`), and blocks for the verdict.
3. Approve the machine in that page — it shows the key fingerprint and the hostname being enrolled. This step needs a human; an agent must stop and ask.
4. `slipfork auth` then prints the server-supplied URL and `[auth] paired …`; later commands dial with the enrolled key.

`slipfork auth` is a desktop command: a `slipforkd` with no `$BROWSER`, `DISPLAY`, or `WAYLAND_DISPLAY` fails pairing closed and prints no URL, so pair from a session that has a browser.

- **Server-supplied URL:** the whole string (`<url>#handle=<handle>`) comes from the server; Slipfork never builds one from the endpoint host, because the browser origin and path may differ.
- **Fragment-only, single-use:** the 32-byte `handle` lives only in the URL fragment, so HTTP access logs and `Referer` headers never record it, and the page strips it from the address bar on load.
- **In-memory pairing:** a pending pairing lives in server memory for that WebSocket only; Ctrl-C or a daemon restart cancels it, so re-run `slipfork auth`.
- **One server identity:** `slipfork auth <ENDPOINT>` fails closed when the URL differs from the daemon's injected `--ospp-endpoint`.
- **Machine key:** the enrolled P-256 key is `~/.local/share/slipfork/device-key-software.json` (`0600`). No secret value is stored beside it.

## What stays local

Before subscribing, `slipforkd` sends the raw `snow.yml` plus `claims`: the `secrets::` keys in the project's vault, the resources it runs locally plus every declared `domains[].id`, and every local-mode `!!` key. A claimed entry is never resolved by the server — its `${{ ... }}` expression comes back untouched and is filled from the local vault. That is how `${{ secrets::LOCAL_PASSWORD }}` wins over CI on your machine while no local value ever leaves it.

Anything **not** claimed is resolved by the server, or refused there if it is missing. Server values whose evaluation touched a secret come back as `{ "encrypted": "<base64>" }` and are decrypted in memory.

## Frames

Every frame is one UTF-8 JSON text message with a `"type"`. Field-level JSON, HPKE parameters, and dump framing live in the normative spec: [`docs/design/ospp.md`](https://github.com/OffworldNexus/slipfork/blob/develop/docs/design/ospp.md).

| Frame | Direction | Purpose |
|---|---|---|
| `hello` | client → server | Protocol `version: 1` and supported auth methods |
| `auth_challenge` / `auth_response` / `auth_ok` | ⇄ | P-256 nonce challenge, proof, and the server's encryption methods |
| `pair_hello` / `pair_challenge` / `pair_begin` | ⇄ | First-time enrolment: ALTCHA proof-of-work plus key proof-of-possession |
| `pair_handle` | server → client | The `url` and single-use `handle` to open |
| `pair_approved` | server → client | The browser approved (or `error`: rejected, already paired, throttled, bad proof) |
| `subscribe` | client → server | Verbatim Git `remote`, ephemeral HPKE public key, raw `snow.yml`, `claims` |
| `env_resolved` | server → client | Per-component environment for the subscription |
| `env_updated` | server → client | Live push after an upstream change; affected services restart in place |
| `unsubscribe` | client → server | The project's last world stopped |
| `dump_request` / `dump_ready` | client ⇄ server | Short-lived HTTPS URL for one HPKE-sealed resource dump |
| `error` | server → client | Coded refusal; handshake, pairing, and auth failures close the socket |

## When it fails

- `[resources.pg_databases.appdb] local = "remote"`, unpaired: `resource 'pg_database.appdb' requires an authenticated OSPP session in Local mode` — run `slipfork auth`, or `up --no-dump` for a clean database.
- `local = "prefer_remote"`, unpaired: the world runs the service locally with a `no_remote_session` reason row (visible under `slipfork up -v`), never a silent half-remote world.
- `--refresh-dump` on a local strategy is refused earlier: `` `--refresh-dump` cannot be combined with a local strategy for pg_databases 'appdb' ``.
- `slipfork auth` on a daemon started without `--ospp-endpoint` fails with `this daemon has no OSPP endpoint` and names the flag to start `slipforkd` with — do that (step 1) and retry.

## Next

- [Local vault & `slipfork secret`](cli.md#slipfork-secret-local-vault) — what the first tier stores and how to manage it.
- [`snow.yml` modes & secrets](../snow-modes.md) — `!!`, `??`, and the vault path.
- [Manifest reference](manifest.md#expressions) — namespaces and resource strategies.
