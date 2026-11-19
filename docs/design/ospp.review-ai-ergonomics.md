# `ai-ergonomics` review — M3.29.4 public secrets docs

Scope: `docs/reference/ospp.md`, `docs/reference/cli.md`, `docs/reference/manifest.md`,
`docs/snow-modes.md`, `docs/getting-started.md`, `docs/index.md`, `README.md`, `zensical.toml`.
Lens: can an LLM coding agent act correctly from these pages alone — two tiers, pairing,
failure→next-command, and no invitation to invent a fallback?

Evidence used: `crates/slipfork/src/{main,daemon,browser,open,vaults,layout}.rs`,
`crates/slipfork/src/bin/slipforkd.rs`, `crates/slipfork-dag/src/{lib,resources}.rs`,
`crates/slipfork-edge/src/{lib,unix_socket,unit,vault}.rs`,
`crates/slipfork/tests/{critical_cli,critical_ospp_daemon}.rs`,
`crates/slipfork-dag/tests/critical_dag.rs`, `plan/nodes/M3/M3.29/M3.29.3/RESULT.md`.
Verified mechanically: `uvx --offline zensical build --strict` → `No issues found` (so every
anchor named in the brief, including `#two-tiers`, `#slipfork-secret-local-vault`, `#pairing`,
`#seed-dumps-and-sealed-bases`, `#slipfork-daemon-unit`, `#expressions`, resolves today), and
`tools/check_docs_honesty.py` covers only subcommand names, not flags or quoted output.

---

## BLOCKER

### B1. The pairing quickstart cannot be followed once `up`/`fork` has started a daemon — the docs never name the ordering constraint or the refusal

`docs/reference/ospp.md:16` (and therefore steps 1–2 at `:28`)

> Pairing needs a server identity, so it needs a `slipforkd` started with `--ospp-endpoint`
> (`wss://`, or `ws://` strictly on loopback). The zero-setup daemon that `up`/`fork` spawn has
> none, and `slipfork auth` fails closed rather than guess a server.

The page is reached *after* `docs/getting-started.md`, which has already run `slipfork up`; that
zero-setup daemon keeps `<state_dir>/run/daemon.sock` for its whole life
(`crates/slipfork-edge/src/unix_socket.rs:72-83` refuses a socket a live daemon answers on), so the
step-1 command an agent copies fails immediately with a string no page documents:

```console
$ slipforkd --state-dir ~/.cache/slipfork --runtime-dir ~/.cache/slipfork/run \
    --listen 127.0.0.1:0 --ospp-endpoint wss://ci.acme-shop.example/ospp
slipforkd: cannot bind the daemon control socket: daemon control socket '/home/acme/.cache/slipfork/run/daemon.sock' is already served by a running slipforkd; refusing to steal it
```

There is no `slipfork daemon stop` verb (`crates/slipfork/src/cli.rs` has only `daemon unit` /
`daemon install`; the only client-side `shutdown` caller is `daemon install`'s handover in
`crates/slipfork/src/daemon_unit_install.rs:395`), so the agent has no documented next command and
will invent one (delete the socket file, `sudo`, a second state dir) — exactly the failure this
lens exists to prevent.

**Replacement wording (`ospp.md:16`):**

> Pairing needs a server identity, so it needs a `slipforkd` started with `--ospp-endpoint`
> (`wss://`, or `ws://` strictly on loopback). Start it **before the first `up`/`fork`**: the
> zero-setup daemon those commands spawn has no endpoint and holds `<state_dir>/run/daemon.sock`
> until it exits, so a second `slipforkd` refuses to start with
> `daemon control socket '<state_dir>/run/daemon.sock' is already served by a running slipforkd; refusing to steal it`.
> If that daemon is already running, stop its worlds (`slipfork down`) and end the process
> (`pkill -x slipforkd`; no `slipfork` verb stops a daemon in `0.1.0`), then start the OSPP daemon
> and pair. `slipfork auth` against an endpoint-less daemon fails closed with
> ``this daemon has no OSPP endpoint; start `slipforkd` with `--ospp-endpoint <wss://...>` ``.

(If `pkill` is not an acceptable user-facing contract, that is a tool gap, not a docs gap: the node
must escalate for a `slipfork daemon stop`-shaped verb rather than leave the quickstart unfollowable.)

### B2. The pairing flow is presented as machine-followable, but the page opens from the **daemon's** desktop and the URL is printed only *after* approval — headless hosts fail with no URL at all

`docs/reference/ospp.md:29-31`

> 2. Run `slipfork auth`. It proves possession of this machine's key and prints the URL the
>    **server** minted.
> 3. Approve the machine in the browser page that opens. The page shows the key fingerprint and
>    the hostname being enrolled.

Two claims are wrong and one impossibility is hidden:

1. `crates/slipfork/src/daemon/ospp.rs` opens the pairing page **inside `slipforkd`**
   (`browser.open_pairing_url`), and the URL is printed by `crates/slipfork/src/main.rs:1320-1321`
   only *after* `client.pair(...)` returns `Ok`. So the URL appears **after** approval: it can
   never be copied to another machine, which is what the prose (print → then approve) implies.
2. When the opener fails, `on_handle` returns `Err`, `slipfork auth` aborts, and **no URL is
   printed at all** (`daemon/ospp.rs`; `crates/slipfork/src/browser.rs:55-75`). A headless
   agent gets `slipfork: OSPP pairing failed: ospp-v1 transport failed: cannot run xdg-open: No such file or directory`
   (or `…: xdg-open exited with <status>`), which no page in this set documents.
3. Approval is inherently human. The page sells steps 1–4 as an agent-followable sequence, so an
   agent will loop on `slipfork auth`, or start constructing `https://<endpoint>/pair#handle=…`
   from the endpoint host — the one thing `ospp.md:33` forbids.

**Replacement wording (`ospp.md:28-31`):**

> 1. Start `slipforkd` with `--ospp-endpoint <wss://…>` (see above: before any world exists).
> 2. Run `slipfork auth`. It proves possession of this machine's key, has **`slipforkd`** open the
>    server-supplied `<url>#handle=…` page through that process's `$BROWSER` (else `xdg-open`), and
>    blocks until the verdict arrives.
> 3. A **human** approves the machine in that page — it shows the key fingerprint and the hostname
>    being enrolled. An agent cannot complete this step for itself; stop and ask for a person at a
>    desktop session.
> 4. The server pushes the verdict over the waiting WebSocket; `slipfork auth` then prints the URL
>    and `[auth] paired p256:… (backend=software:file-0600) account=<you>`. The URL is printed only
>    after approval, so it is never needed on another machine; the next command dials with the
>    enrolled key.
>
> **Headless / no browser:** a `slipforkd` with neither `$BROWSER` nor a desktop session fails the
> pairing closed (`slipfork: OSPP pairing failed: ospp-v1 transport failed: cannot run xdg-open: No such file or directory`)
> and prints no URL. Pair from a desktop session; a `slipforkd.socket`-installed service can never
> pair, because `slipfork daemon unit` renders no `--ospp-endpoint`
> (`crates/slipfork-edge/src/unit.rs:191-196`).

---

## MAJOR

### M1. `slipfork secret set` cannot store a `vars::` override, but the page names `vars::DEBUG` as if it did

`docs/reference/cli.md:78`

> ... an override such as `vars::DEBUG` ...

`FileSecretVault::put` always writes `Namespace::Secrets` (`crates/slipfork/src/vaults.rs:82-101`),
and `validate_secret_key` (`crates/slipfork/src/daemon/env.rs`) accepts `vars::DEBUG` as a
plain printable name. So the literal command an agent derives from this sentence silently
succeeds and creates a junk key:

```console
$ printf '%s' True | slipfork secret set vars::DEBUG
[secret] stored secrets::vars::DEBUG (value never printed)
$ slipfork snow resolve --mode local   # still fails: vars::DEBUG was never written
```

`${{ vars::DEBUG }}` reads `Namespace::Vars`, which has no write path in the CLI (`vars()` is
read-only for claims, `crates/slipfork-edge/src/vault.rs:37-41`). Same false achievability at
`docs/reference/ospp.md:24` ("the local `snow.yml` overrides you store with `slipfork secret set`")
and implied by the example at `ospp.md:41`.

**Replacement wording (`cli.md:78`):** replace `an override such as `vars::DEBUG`` with
`` a `secrets::` override your `modes.local` block reads, such as `secrets::MY_API_TOKEN` ``.

**Replacement wording (`ospp.md:24`):** replace `the local `snow.yml` overrides you store with `slipfork secret set`` with
`` the `secrets::` overrides you store with `slipfork secret set` ``.

**Replacement wording (`ospp.md:41`):** keep the resolution rule but stop implying the CLI writes
both namespaces:

> ... the `secrets::` keys in the project's vault (plus any `vars::` keys already present in it),
> the resources and domains it provisions locally, and every `!!` key.

Escalate separately: developers have no supported way to add a `vars::` override; either document
that limitation or add the write path.

### M2. "your local Postgres password" is an example the runtime never consumes — the world-local `postgres` is `trust`

`docs/reference/ospp.md:24` and `docs/reference/cli.md:78`

> ... backing-service credentials such as your local Postgres password ...

`service_specs` starts every `postgres:<v>` with `POSTGRES_HOST_AUTH_METHOD=trust`
(`crates/slipfork/src/daemon/ospp.rs`), and the rendered `DATABASE_URL` carries no password
(`postgres://127.0.0.1:5432/<id>`, `docs/reference/manifest.md:100`). No code path reads a
Postgres password from the vault (no `POSTGRES_PASSWORD` anywhere in `crates/`). An agent follows
the sentence, stores `PG_PASSWORD`, wires nothing, and gets no error — then reports "credentials
configured" while the database is unauthenticated.

**Replacement wording (`ospp.md:24`):**

> The local `0600` vault (default `~/.local/share/slipfork/vaults/<project>.json`, moved only by
> `$XDG_DATA_HOME`) holds values that are **yours and must survive a restart**: the local
> `secrets::` values your `modes.local` block reads — an API token, a third-party key, an override
> — and every `!!` value generated on first resolve. (The world-local `postgres` runs
> `POSTGRES_HOST_AUTH_METHOD=trust`; it needs no password.)

**Replacement wording (`cli.md:78`):** replace `a local Postgres password, an override such as
`vars::DEBUG`` with `` a local `secrets::` value your `modes.local` block reads (for example
`secrets::MAPS_TOKEN`) ``.

### M3. The reference never documents the global `-v/--verbose`, yet two pages tell the agent to use it

`docs/reference/cli.md:40-44` (Global options table: only `--json`, `--state-dir`, `-h/-V`), while
`docs/getting-started.md:116` says "`-v` adds `[resources]` rows such as `appdb: prefer_remote ->
local (no_remote_session)`" and `docs/reference/ospp.md:66` says "visible under `slipfork up -v`".
`-v` exists as a `global = true` flag (`crates/slipfork/src/cli.rs:35-37`) and is the **only** way
to see the `no_remote_session` row (`crates/slipfork/src/restart.rs:69-79`). An agent reading only
the reference cannot discover it, and `-v` is exactly the diagnostic that distinguishing the two
tiers depends on.

**Add to the table after `--json`:**

| Option | Default | Description |
|---|---|---|
| `-v, --verbose` | `false` | Add the `[resources]` decision rows and the per-node `[lineage]` detail to human output (ignored under `--json`) |

### M4. The three quoted failure strings are not the strings the binary prints, and two of them omit the only next command

`docs/reference/ospp.md:65-67`, `docs/reference/cli.md:140`

- `ospp.md:65` promises `requires an authenticated OSPP session` for `local = "remote"`; the real
  message is ``resource 'pg_databases.appdb' requires an authenticated OSPP session in Local mode``
  (`crates/slipfork-dag/src/lib.rs:65`), with **no remedy**. The `--refresh-dump` message *does*
  carry one: ``the selected seed dump requires an authenticated OSPP session; run `slipfork auth` (or `up --no-dump` for a clean database)``
  (`crates/slipfork/src/daemon/world.rs`).
- `--refresh-dump` on a `local` strategy is a different, earlier refusal: `` `--refresh-dump` cannot be combined with a local strategy for pg_databases 'appdb' ``
  (`crates/slipfork-dag/src/lib.rs:66-67`); neither page quotes it.
- `ospp.md:67` drops the backticks the binary emits: it prints
  ``this daemon has no OSPP endpoint; start `slipforkd` with `--ospp-endpoint <wss://...>` ``
  (`crates/slipfork/src/daemon/ospp.rs`), so an agent matching the page's string verbatim misses.

**Replacement wording (`ospp.md:63-67`):**

> - `[resources.pg_databases.appdb] local = "remote"` unpaired fails closed with
>   ``resource 'pg_databases.appdb' requires an authenticated OSPP session in Local mode`` — start
>   the OSPP daemon and run `slipfork auth`, or use `local` / `prefer_remote`.
> - `--refresh-dump` unpaired fails closed with
>   ``the selected seed dump requires an authenticated OSPP session; run `slipfork auth` (or `up --no-dump` for a clean database)``.
> - `--refresh-dump` against a `local` strategy is refused without touching a session:
>   `` `--refresh-dump` cannot be combined with a local strategy for pg_databases 'appdb' ``.
> - `local = "prefer_remote"` degrades to the world-local service with a `no_remote_session` reason
>   row (visible under `slipfork up -v`), never a silent half-remote world.
> - `slipfork auth` on a daemon started without `--ospp-endpoint` fails with
>   ``this daemon has no OSPP endpoint; start `slipforkd` with `--ospp-endpoint <wss://...>` ``.

**Replacement wording (`cli.md:140`):** replace the single substring sentence with the same three
verbatim messages above, then keep the final sentence about needing a provider.

---

## MINOR

### m1. The quickstart console block puts a foreground daemon ahead of the next command

`docs/reference/ospp.md:6-13` shows `slipforkd …` followed by `slipfork auth` in one transcript.
`slipforkd` "runs in the foreground" (`cli.md:144`), so a literal copy hangs at line 7. **Fix:** add
`# terminal 1` above the `slipforkd` line and `# terminal 2 (or append &)` above `slipfork auth`,
or append `&` to the daemon line.

### m2. The `exec` example never shows the `snow.yml` declaration that makes the variable exist

`docs/reference/ospp.md:12` prints `injected` for `STRIPE_SECRET_KEY`, but the server only resolves
expressions present in the manifest (`claims`/`subscribe` carry the raw `snow.yml`,
`docs/design/ospp.md:109-146`), and `exec` injects a component's resolved environment. **Fix:** add
one line before the block: "`snow.yml` must declare it, e.g.
`modes.all.envs.api: { STRIPE_SECRET_KEY: '${{ secrets::STRIPE_SECRET_KEY }}' }`; an undeclared
variable is simply absent from the shell."

### m3. Pairing is documented as repeatable, but it is first-time enrolment

`docs/reference/ospp.md:28-31` reads as a command an agent may re-run whenever secrets look stale.
The daemon always dials `pair` (`crates/slipfork/src/daemon/ospp.rs`), and the spec's verdicts
include `already_paired` (`docs/design/ospp.md:107`). **Fix:** append to step 4: "`slipfork auth`
is first-time enrolment: once the key is enrolled the daemon authenticates with it on every dial,
so do not re-run `slipfork auth` to 'refresh' secrets — `env_updated` pushes them."

### m4. `slipfork auth <ENDPOINT>` compares byte-for-byte, so a normalising agent gets a confusing refusal

`docs/reference/ospp.md:36` / `cli.md:73`. The comparison is `requested != client.endpoint().as_str()`
(`crates/slipfork/src/daemon/ospp.rs`), so a trailing slash or a `ws://`/`wss://` swap fails.
**Fix:** at `ospp.md:36` add: "the argument must match the injected `--ospp-endpoint` byte for byte
(no trailing slash, same scheme); omit it and the daemon uses its own endpoint."

### m5. `!!` is not local-mode-only

`docs/reference/ospp.md:24` and `docs/reference/cli.md:78` say "the local-mode `!!` values". Every
`!!` persists in the vault regardless of mode (`slipfork secret`/`!!` share `Namespace::Secrets`,
`crates/slipfork/src/vaults.rs:82-101`; `crates/slipfork/src/main.rs:1455-1460` reports
`[secrets] ensured secrets::<KEY>`). **Fix:** drop "local-mode" in both places — "every `!!` value
generated on first resolve".

---

## NIT

- `docs/snow-modes.md:96` tells the reader to set `$XDG_DATA_HOME` because "`--state-dir`/`--runtime-dir`
  do not move it"; `--runtime-dir` is a `slipforkd`-only flag, so the sentence names a flag a
  developer never types here. Use "`--state-dir` does not move it".
- `docs/reference/cli.md:4-7` and `:117-120` print the identical `no slipforkd` transcript twice.
  Keep the one in "Failure modes you will actually see" and reference it from the introduction.

---

## Checked and clean (no action)

- Anchors and cross-page links: `uvx --offline zensical build --strict` → `No issues found`;
  `#two-tiers`, `#pairing`, `#slipfork-secret-local-vault`, `#seed-dumps-and-sealed-bases`,
  `#slipfork-daemon-unit`, `#expressions`, `#2-slipforktoml-…` all resolve.
- `no_remote_session` is exactly the wire spelling (`serde(rename_all = "snake_case")` on
  `ResourceReason::NoRemoteSession`, `crates/slipfork-dag/src/resources.rs:266-270`, rendered
  through `json_label` at `crates/slipfork/src/daemon/ospp.rs`), and it is genuinely `-v`-only
  (`crates/slipfork/src/restart.rs:69-79`).
- `[auth] paired … (backend=software:file-0600)` and `[secret] stored secrets::<KEY> (value never
  printed)` match the code byte for byte (`main.rs:1320-1329`, `main.rs:844`).
- Vault path/mode/`$XDG_DATA_HOME`-only (`layout.rs:121-127`), device key path and `0600`
  (`layout.rs:162-167`), "never written to disk" for remote values, and `--refresh-dump` /
  `--no-dump` / `--resources local` implications all match the implementation.
- No page claims an ephemeral in-memory session secret for `!!` — the superseded M3.29.4 design
  bullet at `plan/nodes/M3/M3.29/M3.29.4/NODE.md:38` ("generates an ephemeral **session** secret")
  is **not** reflected in any published page, which is correct; do not reintroduce it.
- Two tiers are distinguishable without internals: first tier is written only by
  `slipfork secret set` / `!!`, and the second is never written anywhere (ospp.md:24, cli.md:78,
  index.md:34).

VERDICT: CHANGES-REQUIRED
