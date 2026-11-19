# Tunnel Weaver Naming, Device-Key Auth, Pairing & systemd Socket-Activation Facts

- **Date:** 2026-11-15
- **Sources (primary):**
  - `~/offworld-nexus/tunnel-weaver` @ `c43d154` (`[WVR-177] Make the flaky CI tests deterministic (#41)`) — source, ADRs, operations docs, golden unit files.
  - `~/offworld-nexus/tpmd` working tree (`tpmd 0.1.1`) — `tpmd-ipc`, `tpmd-proto`, `tpmd-client`, dummy backend.
  - Live probes on this host (systemd 257.13, kernel 6.12.117, LXC container).
- **Reproduce live parts:** commands in each section below; no compiled test was run in `tunnel-weaver` (no `target/` was present and the workspace was not built for this note).

> **Premise correction (measured, not inferred):** `plan/nodes/M2/LOG.md:17` directs "Tunnel Weaver 64-char service label truncation". The actual service-name cap in Tunnel Weaver is **31 bytes**, not 64 (`crates/weaver-proto/src/control.rs:97`). The installed `weave 0.1.0` enforces the same number (probe in §1.4). Any truncation scheme must target **31**, and the combined label then lands on exactly **63** octets.

---

## 1. Hostname / label limits

### 1.1 The three name caps (exact)

| Field | Rule | Max | Source |
|---|---|---|---|
| `service` | `^[a-z0-9]+(-[a-z0-9]+)*$` (no empty run, so no leading/trailing/doubled dash; lowercase only) | **31 bytes** | `crates/weaver-proto/src/control.rs:99-115`; constant at `:95-97` |
| `person` | `^[a-z0-9]{1,15}$` | **15 bytes** | `crates/weaver-server/src/store/names.rs:16-29` |
| `machine` | `^[a-z0-9]{1,15}$` | **15 bytes** | `crates/weaver-server/src/store/names.rs:16-29` |
| combined label | `<person>-<machine>-<service>` | **exactly 63 bytes at worst case** | arithmetic over the caps above; composition at `crates/weaver-server/src/store/names.rs:36-45` |

`SERVICE_NAME_MAX` is documented as the remainder of a 63-byte DNS label after person and machine:

```rust
/// Longest service name: what is left of a 63-byte DNS label once the person
/// and machine (up to 15 bytes each) and their two dashes are in front of it.
pub const SERVICE_NAME_MAX: usize = 31;
```
(`crates/weaver-proto/src/control.rs:95-97`)

Unit tests pin the boundary — 31 accepted, 32 rejected, `-leading`, `trailing-`, `a--b`, `Web`, `with.dot`, `with_underscore` rejected (`crates/weaver-proto/src/control.rs:121-134`).

Server-side person/machine creation re-checks the same rule and rejects with an explicit message (`crates/weaver-server/src/store/mod.rs:576-581` for person, `:651-658` for machine); service creation rejects with the exact charset + length wording (`crates/weaver-server/src/store/mod.rs:960-971`).

### 1.2 Why the combined label can never exceed 63

Worst case is `15 + 1 + 15 + 1 + 31 = 63`, i.e. the entire RFC 1035 label budget is allocated on purpose:

- `flat_hostname()` simply formats `{person}-{machine}-{service}.{root}` after lowercasing every part (`crates/weaver-server/src/store/names.rs:36-45`).
- `Identity::hostname()` is the only production caller (`crates/weaver-server/src/tunnel/identity.rs:62-67`), reached from `TunnelRegistry::register_service()` (`crates/weaver-server/src/tunnel/registry.rs:337-348`).
- There is **no explicit `len() <= 63` check** anywhere in the relay; the invariant holds only because the three component caps are enforced at creation time. A grep for `63` over `weaver-server`/`weaver-proto` finds only unrelated `WVR-163` ticket strings and the setup domain validator (`crates/weaver-server/src/setup/interactive.rs:49,57`).
- The DNS responder accepts exactly one label under the apex and only if every byte is `[A-Za-z0-9-]` (`crates/weaver-server/src/dns/mod.rs:295-311`), so a malformed/too-long label is `NXDOMAIN` rather than a tunnel.

ADR 0008 fixes the shape as a hard invariant because the wildcard cert `*.<root>` matches exactly one label (`docs/decisions/0008-flat-hostnames-wildcard-cert.md:20-27`).

### 1.3 Truncation: Tunnel Weaver has none — Slipfork must supply it

There is no truncation helper in `tunnel-weaver` (grep for `truncat` hits only log/auth-string truncation, e.g. `crates/weaver-server/src/account/auth/manager.rs:703`). What the relay provides is only collision-safe **suffix numbering** for person/machine slugs:

- `candidates(base)` yields `base`, then `<truncated base><n>` for `n = 2,3,…`, truncating to `PERSON_MACHINE_MAX - suffix.len()` and appending a bare digit (`crates/weaver-server/src/store/names.rs:105-121`), with tests asserting `"a"*15` → `"a"*14 + "2"` → `"a"*13 + "10"` stays valid (`:166-178`).
- The doc comment states the reason the suffix is a *bare digit after truncation*: every candidate stays `^[a-z0-9]{1,15}$` and **contains no dash**, so the flat label still splits at its first two dashes (`:105-110`).

Consequences for Slipfork's `<prefix>-<fixed_id>` scheme (all directly implied by the cited validator at `control.rs:99-115`, not by any weaver helper):

1. Budget the **whole** service at 31 bytes: `len(prefix) + 1 + len(fixed_id) <= 31`.
2. `fixed_id` must be `[a-z0-9]+`; if it can start with a digit that is fine, but it must not be empty and must not contain `-` unless it is itself dash-separated runs.
3. Truncate `prefix` to `31 - 1 - len(fixed_id)`, then **strip a trailing `-` from the truncated prefix** (and re-strip if the next char was also `-`), otherwise the join produces `a--<id>` or `-<id>`, both rejected (`control.rs:108-113`).
4. If the stripped prefix becomes empty, drop it entirely rather than emitting a leading dash.
5. Since person and machine are relay-assigned and each capped at 15, a valid service alone guarantees the combined label is `<= 63`; Slipfork never needs to know person/machine to be length-safe.

### 1.4 Live confirmation from the installed client

```
$ weave --version
weave 0.1.0 (protocol v1)

$ weave start "$(printf 'a%.0s' $(seq 32))=8080"
error: invalid service name 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa': lowercase letters and digits,
single dashes between them, at most 31 chars
```

A 31-byte name passes validation and proceeds to relay connection (it blocked waiting for a paired relay until killed), confirming the accepted boundary is 31.

---

## 2. Device key: algorithm, `KeyId`, `tpmd`

### 2.1 Algorithm and key identity

| Fact | Value | Source |
|---|---|---|
| Algorithm | **ECDSA P-256 + SHA-256** (`EcdsaP256Sha256`) | `tpmd/crates/tpmd-proto/src/types.rs:17-25`; provisioned in `crates/weave/src/tpm.rs:42-47` |
| Public key encoding | **SEC1 compressed, 33 bytes** | `tpmd-proto/src/types.rs:19-27`; `crates/weaver-mux/src/wire.rs:46-50` |
| Signature encoding | **raw fixed-width `r‖s`, 64 bytes** (not DER) | `tpmd-proto/src/types.rs:30-34`; `crates/weaver-mux/src/wire.rs:105-111` |
| `KeyId` derivation | **the public key bytes themselves — no hash, no separate id** | `tpmd-proto/src/types.rs:111-120`; `wire.rs:41-50` |
| Text form | `p256:<66 lowercase hex>` / `ed25519:<64 hex>` | `wire.rs:52-76` (Display), `:78-104` (FromStr) |

`tpmd-proto` states it explicitly: "A key's identity: **its fingerprint**, which is exactly the public key in the algorithm's compact encoding… which is also the value Tunnel Weaver uses as its handshake `KeyId`. There is no separate 'id' concept, and keys are never listed." (`tpmd/crates/tpmd-proto/src/types.rs:111-118`)

`weaver-mux` supports two algorithm variants (`Ed25519([u8;32])`, `P256([u8;33])`) and rejects any mismatch between `KeyId`, `PublicKey` and `Signature` variants (`crates/weaver-mux/src/auth.rs:17-23`, `:83-110`). Production `weave` only ever provisions P-256 (`crates/weave/src/tpm.rs:42-47`); Ed25519 exists for adapters/tests.

### 2.2 Where signing happens, and the exact signed bytes

`tpmd` signs an **arbitrary message** and hashes it internally:

```rust
/// The daemon SHA-256-hashes `message` and ECDSA-signs the digest; the
```
(`tpmd/crates/tpmd-client/src/lib.rs:298-311`; the dummy backend annotates the same contract: "`Signer::sign` applies SHA-256 to `message` internally, which is exactly the contract `weaver_mux::auth::verify` expects", `tpmd/crates/tpmd-backend-dummy/src/lib.rs:164-169`).

Production `weave` never holds a private key: it asks `tpmd` over async IPC through `tpmd-client` (`crates/weave/src/tpm.rs:1-20`, `crates/weave/src/key.rs:1-8`). Security levels are `file < keyring < sealed < hardware` (`tpmd-proto/src/types.rs:66-99`); `weave`'s default floor is `sealed` with `allow_below_floor: false` (`crates/weave/src/key.rs:14-30`), overridable by `--min-key-security` / `--allow-below-floor` (`docs/architecture.md:566-575`).

### 2.3 `tpmd` socket and fallback behavior

| Fact | Value | Source |
|---|---|---|
| Socket file name | `tpmd.sock` | `tpmd-ipc/src/lib.rs:45` |
| Per-user endpoint | `$XDG_RUNTIME_DIR/tpmd/tpmd.sock` (macOS: `$TMPDIR/tpmd/tpmd.sock`) | `tpmd-ipc/src/lib.rs:96-110`; `tpmd-ipc/src/unix.rs:25-27` |
| System endpoint (Linux only) | `/run/tpmd/tpmd.sock` | `tpmd-ipc/src/lib.rs:55`, `:113-142` |
| Client candidate order | **per-user first, then system** | `tpmd-ipc/src/lib.rs:144-152` |
| Socket modes | owner-only `0600` (user daemon) vs `0666` (shared system daemon); **reachability is not authorization** — every op is scoped to the kernel-authenticated peer principal | `tpmd-ipc/src/lib.rs:57-82`, `:168-175` |
| systemd handover | `listenfd::ListenFd::from_env()` → `take_unix_listener(0)`; the inherited listener is set **non-blocking** before tokio adoption | `tpmd-ipc/src/unix.rs:29-58` |

`weave`'s adapter behavior (`crates/weave/src/tpm.rs`):

- Namespace `"tunnel-weaver"` (`:27`), so keys are app-scoped inside `tpmd`.
- Tries every `Endpoint::client_candidates()` in order, returning the first that **answers**; `Unavailable` falls through to the next, any other answer is returned (`:105-140`).
- Requests are `KeyRequest::ResumeOrCreate { key_id, provision }` when a remembered P-256 `KeyId` exists, else `Create(provision)` (`:38-52`), i.e. get-or-create is atomic in the agent.
- `StoreClosed` (locked session key store) is **waited out**, first with a zero-budget probe then re-issued with a 1 h budget, looping until success or a different error; the user ends the wait with Ctrl-C (`:10-18`, `:84-100`, `:114-120`).
- No file/keyring fallback in `weave`: a missing agent is `KeyError::Unavailable` surfaced to the user; a below-floor machine is `KeyError::BelowFloor { required, available }` (`crates/weave/src/key.rs:46-63`).

Live host confirmation: `/usr/bin/tpmd` 0.1.1 is installed, `tpmd.service` is `active (running)`, triggered by `tpmd.socket`, running as `User=tss` with `CapabilityBoundingSet=`/`AmbientCapabilities=` empty and `TPMD_MIN_SECURITY=sealed`; `/run/tpmd/tpmd.sock` exists with mode `srw-rw-rw-` (system/`LocalUsers` access). The per-user path `/run/user/1000/tpmd/` does **not** exist on this host.

---

## 3. CHALLENGE → HELLO → WELCOME transcript

Transport: WebSocket over TLS (`wss://<admin-host>:<port>/_weaver/connect`, subprotocol `weaver-mux-v1`), path and subprotocol constants at `crates/weave/src/client.rs:26-29`, dialed at `:183-216`. `server_name` in the transcript is **the hostname string the client dialed**, and both sides must pass the exact same string (`crates/weaver-mux/src/config.rs:112-115`); the client passes `host.host.clone()` (`crates/weave/src/client.rs:215-219`).

Exact byte layout (`crates/weaver-mux/src/auth.rs:54-77`):

```
"weaver-mux-v1" ‖ nonce_s(32) ‖ nonce_c(32) ‖ server_name_bytes [‖ channel_binding(32)]
```

- Domain-separation prefix `b"weaver-mux-v1"` (`crates/weaver-mux/src/wire.rs:30-31`).
- Everything except `server_name` is fixed width, so the concatenation is unambiguous without length prefixes as long as both sides agree on whether the channel binding is present (`auth.rs:57-61`).
- The client does not sign a hash of its own: it hands these exact bytes to `Signer::sign`, which is `tpmd` (SHA-256 inside the agent, §2.2).

Sequence (stream 0, control frames):

| Step | Sender | Payload | Source |
|---|---|---|---|
| 1 | server | `CHALLENGE { nonce_s: [u8;32] }` — 32 bytes from the server RNG at connection construction | `crates/weaver-mux/src/connection.rs:120-130`; struct `wire.rs:113-117` |
| 2 | client | `HELLO { version: u16, key_id, nonce_c: [u8;32], sig }`; `version` is deliberately the first field so any server can read it | `crates/weaver-mux/src/connection.rs:415-450`; struct `wire.rs:119-135` |
| 3 | server | look up `Verifier::public_key(key_id)` → `REJECT { UnknownKey }` if absent; rebuild the transcript with its own `nonce_s` and verify; `REJECT { BadSignature }` on failure | `crates/weaver-mux/src/connection.rs:462-500` |
| 4 | server | `WELCOME { version, params: { max_frame, initial_window, compression_allowed, max_message } }`; handshake is `Done` only now; any frame before WELCOME is a state violation | `crates/weaver-mux/src/connection.rs:402-410`, `:500-506`; struct `wire.rs:137-165` |

- Version negotiation: min = max = **1** today; a higher client offer is clamped to 1, an offer below 1 is rejected with `UnsupportedVersion { min, max }` (`crates/weaver-mux/src/wire.rs:25-28`, `crates/weaver-mux/src/handshake.rs:22-43`).
- Server-side revocation is polled lazily every `Config::reverify_interval` through `Verifier::still_valid`, defaulting to `true` (`crates/weaver-mux/src/auth.rs:41-51`).
- `Signer`/`Verifier` are async traits precisely because real implementations do I/O (TPM agent, database); the mux itself stays sans-IO (`auth.rs:24-40`).

### TLS channel binding: implemented, **not enabled in production**

`Config::channel_binding: Option<[u8;32]>` is "TLS exporter value mixed into the transcript when both sides set it" (`crates/weaver-mux/src/config.rs:122-125`), defaults to `None` (`:141`), and is appended only when present (`auth.rs:62-76`), on both client (`connection.rs:421-433`) and server (`connection.rs:480-491`). A mismatch fails the handshake (test `crates/weaver-mux/tests/handshake.rs:125-145`).

Grep over `crates/*/src` for `channel_binding` outside `weaver-mux` returns **nothing**: neither `weave` nor `weaver-server` sets it. So today the transcript authenticates the device key to the server but does **not** cryptographically bind the signature to the TLS channel; an on-path attacker cannot forge a signature but a relay-terminating proxy could in principle relay a signature to a different TLS session with the same `server_name` and nonces. Slipfork adopting this design verbatim inherits that same posture; setting the exporter on both ends is the available (already-implemented) hardening.

---

## 4. Pairing: WebSocket schema and handshake

Pairing deliberately does **not** use the mux: it is a plain JSON WebSocket on the admin host, because the browser API is JSON already (`crates/weaver-proto/src/pair.rs:1-17`).

| Constant | Value | Source |
|---|---|---|
| Subprotocol | `weaver-pair-v1` | `crates/weaver-proto/src/pair.rs:28-29` |
| Channel path | `/api/account/pair/channel` | `pair.rs:31-32` |
| Browser API | `POST /api/account/pair/{lookup,accept,reject}` | `pair.rs:34-39` |
| PoW purpose / flow | `pair` (ALTCHA) | `pair.rs:41-42` |
| Message cap | 16 KiB (`max_message_size` and `max_frame_size`) | `crates/weaver-server/src/account/pairing.rs:44-56` |
| `Begin` deadline | 60 s after the challenge | `pairing.rs:44-47`, `:247-250` |
| Handle | 32 random bytes, lowercase hex (64 chars), single use | `pairing.rs:122-141`; doc `pair.rs:80-86` |

Wire messages (JSON, tagged `"type"`, snake_case — `pair.rs:73-125`):

1. server → CLI `Challenge { challenge: altcha::Challenge }` (signed ALTCHA challenge, first message always).
2. CLI → server `Begin { proof: altcha::Payload, key_id: KeyId, target: Target }`, where `Target` is `New { hostname }` or `Existing { machine_id }` (`pair.rs:44-60`, `:61-73`).
3. server → CLI `Handle { handle }`.
4. server → CLI exactly one terminal `Approved { machine_id, person, machine }` or `Failed { code, message }` with codes `bad_proof | bad_key | already_paired | bad_request | throttled | unavailable | rejected | not_your_machine` (`pair.rs:75-125`).

Browser side: `LookupResponse::Waiting { fingerprint, account, person: Proposed, machine: Proposed }` plus `Accepted`/`Rejected` variants; `accept` may carry `person_slug` / `machine_name` overrides (`pair.rs:128-200`).

Operational properties verified in code/docs:

- The pending pairing is **in-memory only** and lives exactly as long as the CLI's WebSocket; a relay restart ends it (`docs/architecture.md:616-620`).
- Handle is a capability carried only in the URL **fragment** (`https://<admin>/pair#handle=<hex>`), so it never reaches access logs or `Referer` (`crates/weave/src/pair.rs:58-63`; `docs/operations.md:225-235`).
- A key maps to at most one machine; `Begin` for an already-paired key is refused unless it targets `Existing { machine_id }`, which moves the machine onto the new key and closes the old key-log row (`docs/architecture.md:620-640`; store `replace_machine_key` at `crates/weaver-server/src/store/mod.rs:700-760`).

### What the OSPP WebSocket protocol can mirror cleanly

These are the verified mechanics Slipfork can copy without inventing anything: JSON text frames + a tagged enum; server-speaks-first random nonce; client signs a fixed domain-separated transcript with a device key whose id **is** the public key; server looks the key up and verifies; explicit version field first; bounded message size and a bounded first-message deadline; capability handle delivered out-of-band (fragment) rather than in the ready-to-use URL. The mux's stream/frame layer is **not** needed for OSPP unless Slipfork wants multiplexed streams — `pair.rs` shows the JSON-only precedent.

---

## 5. systemd socket activation: verified mechanics and live host constraints

### 5.1 Live host facts

| Probe | Result |
|---|---|
| `systemctl --version` | `systemd 257 (257.13-1~deb13u1)`, features include `+SECCOMP`, `+TPM2`, `+BPF_FRAMEWORK`, `-GNUTLS +OPENSSL` |
| `systemctl is-system-running` | `running` (rc 0); PID 1 is `systemd`; `systemd-detect-virt` → `lxc` |
| `systemctl --user status` | rc **0**, `State: degraded`, `Failed: 1 units` (`xfce4-notifyd.service`); 88 units loaded; `Linger=yes`; `/run/user/1000/systemd/private` exists |
| `cat /proc/sys/net/ipv4/ip_unprivileged_port_start` | **1024** (`sysctl net.ipv4.ip_unprivileged_port_start` agrees) |
| Capabilities of this shell | `CapPrm: 0`, `CapEff: 0`, `CapBnd: 000001ffffffffff` |
| Bind `127.0.0.1:80` as this user | `systemd-socket-activate -l 127.0.0.1:80 /bin/true` → `Failed to listen on 127.0.0.1:80: Permission denied` (rc 1); `nc -l 127.0.0.1 80` → `Permission denied` |
| Socket activation handover | see §5.2 — `LISTEN_PID == child PID`, `LISTEN_FDS=1`, fd `3` is a listening socket |

Consequence: on this host **no unprivileged component — including `systemd --user`, which runs with the user's empty capability sets — can bind port 80**. `systemd-socket-activate` is the same unprivileged code path a user manager uses, and it fails identically. A port-80 `slipforkd` therefore needs one of: a **system-level** `.socket` unit (root manager binds, then drops to the user), `setcap cap_net_bind_service=+ep` on the binary, a lowered `net.ipv4.ip_unprivileged_port_start`, or a high port plus redirect. The `--listen` unprivileged-test flag in `plan/nodes/M2/LOG.md:17` is the only one of these that needs no privilege at all.

### 5.2 `LISTEN_FDS` / `LISTEN_PID` mechanics (live)

`systemd-socket-activate` execs the child **only on the first connection**, with:

```
$ systemd-socket-activate -l 127.0.0.1:38124 /bin/sh -c 'echo PID=$$ LISTEN_PID=$LISTEN_PID LISTEN_FDS=$LISTEN_FDS; ls -l /proc/self/fd/3'
Listening on 127.0.0.1:38124 as 3.
Communication attempt on fd 3.
Execing /bin/sh (...)
PID=866762 LISTEN_PID=866762 LISTEN_FDS=1
lrwx------ 1 remy remy 64 Nov 15 16:19 /proc/self/fd/3 -> socket:[10309228]
```

So: sockets start at **fd 3**, `LISTEN_FDS` is the count, `LISTEN_PID` is the **child's** PID and must be compared before trusting the fds, and the fds are **blocking** (the daemon must set non-blocking itself).

### 5.3 How `weaver-server` consumes activation (reference implementation)

`crates/weaver-server/src/server/listener.rs:50-160`:

- Reads `LISTEN_FDS` and `LISTEN_PID` from the environment; if `LISTEN_PID != getpid()`, logs and **falls back to self-bind** (`:57-68`).
- Parses the count, requires at least 2 (`:70-78`).
- Iterates `for fd in 3..3 + num_fds` (`:85`), reconstructs a `socket2::Socket` from each raw fd, classifies by `SO_TYPE` (`DGRAM` vs stream) and by `getsockname().port()`; datagrams must be port 53, streams are matched against `listen_http.port()`, `listen_https.port()` and `DNS_PORT`, anything else is a hard activation error (`:86-143`).
- **Every** adopted socket is explicitly `set_nonblocking(true)` before `TcpListener::from_std` / `UdpSocket::from_std` (`:103-135`) — the same trap `tpmd` documents at `tpmd-ipc/src/unix.rs:50-58` ("systemd passes its sockets in *blocking* mode… the first accept after a connection does not return `WouldBlock`").
- Missing HTTP or HTTPS after scanning is an error, not a silent fallback (`:145-149`).
- Without `LISTEN_FDS` the server self-binds dual-stack with `SO_REUSEADDR`; DNS is best-effort and only on explicit relay addresses (`:1-7`, `:50-55`).

### 5.4 The unit pattern Tunnel Weaver ships (golden files)

`crates/weaver-server/tests/golden/weaver-server.socket:1-16`:

```ini
[Socket]
ListenStream=[::]:80
ListenStream=[::]:443
ListenDatagram=203.0.113.7:53
ListenStream=203.0.113.7:53
BindIPv6Only=both
NoDelay=true
Backlog=1024
Service=weaver-server.service
```

`crates/weaver-server/tests/golden/weaver-server.service:1-35`: `Type=notify`, `User=weaver`, `Requires=weaver-server.socket`, `CapabilityBoundingSet=` (empty), `NoNewPrivileges=yes`, `ProtectSystem=strict`, `PrivateTmp=yes`, `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX`, `SystemCallFilter=@system-service`, `LimitNOFILE=65536`; `docs/operations.md:115-122` states "systemd owns the binds; the service runs with an empty `CapabilityBoundingSet=`" and that without activation a container needs `CAP_NET_BIND_SERVICE`. ADR 0008 requires the same for 53 (`docs/decisions/0008-flat-hostnames-wildcard-cert.md:75-77`).

`weave-server setup` renders this unit from code (`crates/weaver-server/src/setup/units.rs:76-124`), so the golden file and the generator are the same contract.

---

## 6. Native binaries present on this host

| Binary | Path | Version / note |
|---|---|---|
| `psql` | `/usr/bin/psql` | PostgreSQL client 18 (server package installed) |
| `postgres`, `initdb`, `pg_ctl`, `createdb`, … | `/usr/lib/postgresql/18/bin/` (not on `PATH`) | `postgres (PostgreSQL) 18.7 (Debian 18.7-1.pgdg13+2)`; `dpkg -l` shows `postgresql-18` + `postgresql-18-jit` |
| `redis-cli` | `/usr/bin/redis-cli` | **`redis-server` absent** |
| `weed` / `seaweedfs` | — | absent |
| `socat` | — | absent |
| `caddy`, `nginx` | — | absent |
| `docker` | `/usr/bin/docker` | daemon socket `/var/run/docker.sock` present (`root:docker`, `srw-rw----`); user `remy` is in group `docker` |
| `nft`, `iptables`, `ip`, `unshare`, `nsenter` | `/usr/sbin`, `/usr/bin` | present |
| `nc`, `jq`, `curl` | `/usr/bin` | present |
| `tpmd` | `/usr/bin/tpmd` | 0.1.1, system service active |
| `weave` | `/usr/local/bin/weave` | `weave 0.1.0 (protocol v1)` |
| `cargo` / `rustc` / `uv` | `~/.cargo/bin`, `~/.local/bin` | `rustc 1.100.0`, `uv 0.13.8` |

Host: kernel `6.12.117+deb13-amd64`, `x86_64`, LXC container with systemd PID 1, 10 CPUs, 48 GiB RAM, `/` on the 800 GiB XFS volume, `/tmp` a 32 GiB tmpfs.

---

## 7. Unverified / Requires Host Access

- **`weaved` IPC contract:** `tunnel-weaver` ships no `weaved` daemon, no registration wire schema, and no socket path. Nothing in this repository defines a `weaved` IPC; `H-006` remains the blocker for a `WeavedRegistrar` wire adapter. (Grep for `weaved` in `tunnel-weaver` returns only docs about the relay.)
- **`Endpoint::client_candidates()` Windows named-pipe form and macOS `$TMPDIR` fallback:** read from `tpmd-ipc/src/lib.rs:96-110` as source, not exercised on this Linux host.
- **Channel-binding exporter plumbing:** the mux accepts a 32-byte exporter value, but no code computes or supplies one (neither side sets `Config::channel_binding`); how a future adapter would extract it from `rustls` is not implemented anywhere in either repository.
- **Root/system-level port-80 socket activation:** the mechanism is proven for unprivileged ports (§5.2) and the unit pattern is proven in Tunnel Weaver's golden files, but no root-owned unit could be installed on this LXC host to observe `:80` activation end-to-end. Only the *failure* of the unprivileged path was observed.
