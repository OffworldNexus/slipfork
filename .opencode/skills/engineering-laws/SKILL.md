---
name: Engineering Laws (Legality, Upward IoC/DI, Zero Pre-1.0 Compat)
description: MANDATORY for all Slipfork design, coding, planning, and review tasks. Defines the three non-negotiable engineering invariants and exact code smells rejected by review-legality, review-ioc, and review-cruft.
---

# The Three Engineering Iron Laws

Every artifact in Slipfork is audited by three dedicated narrow-context smell reviewers (`review-legality`, `review-ioc`, `review-cruft`). Memorize these rules and smells before writing a line of design or code.

---

## Law 1: Zero "Illegal" Solutions — Block on Missing Inputs (`review-legality`)

An **Illegal Solution** is any code, spec, or workaround that makes a task appear "done" locally while creating negative value, technical debt, or false guarantees afterwards.

### Forbidden Smells (`BLOCKER`):
- **Inventing missing inputs:** Guessing design values (`[derived]`, `[guessed]`), fabricating API schemas, or inventing placeholder behavior when an external system, credential, or host capability (`incusd` socket, remote secret vault) is unavailable.
- **Silent degraded fallbacks:** Catching a missing capability (e.g., CoW reflink unsupported, Incus unreachable, missing required `secrets::KEY` with no `??`/`!!`) and silently running unisolated on the host or substituting an empty string. **Fail closed with a clear, actionable error!**
- **Contract monkey-patching:** Bypassing framework/library configuration contracts with raw `std::env::var` / `os.environ` side-doors, mutating private internals, or weakening lints (`#[allow(...)]`, `# noqa`, `@ts-ignore`) instead of fixing the root contract.
- **Ignoring declared dependencies:** Hand-rolling YAML/TOML/CLI parsers, temp-dir cleaners, or UI/protocol helpers already provided by workspace crates or `Cargo.toml`/`package.json`.

### Mandatory Action When Blocked:
Stop immediately, leave no half-baked shim on disk, and return `STATUS: blocked` with the exact missing input so `lead`/`conductor` files:
```bash
uv run --no-project tools/plan.py --agent <role> human add --kind missing-input "<exact missing resource/credential/decision and how to provide it>" --node <ID>
```

---

## Law 2: Strict IoC / DI & Upward Direction Challenge (`review-ioc`)

LLMs naturally think **downwards** (*"how do I bolt feature X onto existing files?"*) and fail to think **upwards** (*"what is the global trait/DI architecture across all backends and languages, and is the specification complete?"*).

### Mandatory IoC / DI Rules:
1. **Single Composition Root:** Only `crates/slipfork/src/main.rs` (for the CLI/daemon) and `packages/opencode-slipfork/src/index.ts` (for the OpenCode plugin) read `std::env` / `process.env`, bind real OS sockets/filesystems, and wire concrete adapters into traits.
2. **Zero Global State:** No mutable `static`, `lazy_static!`/`OnceLock` caches hiding I/O, or module-level mutable maps. Pass collaborators explicitly through constructors.
3. **Capability-Honest Traits, Never `match kind` Sprawl:**
   - Adding a new hypervisor (`IncusEngine`, `ReflinkNetnsEngine`), secret backend (`FileVaultProvider`, `HttpSecretProvider`), or language runner (`PytestAdapter`, `PlaywrightAdapter`, `PhpUnitAdapter`, `CargoAdapter`) **must** implement the shared domain trait (`WorldEngine`, `SecretProvider`, `TestAdapter`) and pass the **shared conformance test suite** for that trait.
   - Never build a parallel pipeline alongside an existing one.

### Mandatory Upward Direction Challenge:
**If you cannot draw a clean, non-leaky DI tree for a feature, there is a blind spot in the specification.**
- Symptoms of a spec blind spot: a trait method needs a `downcast_ref`, an opaque `extra: Map<String, Value>` bag, boolean mode flags (`is_local_hack: bool`), or incompatible lifecycle assumptions between two implementations.
- **Action:** Do NOT paper over the blind spot! Stop and escalate upwards (`STATUS: blocked` -> `plan.py human add --kind direction-challenge`) stating: (1) the exact DI conflict, (2) 2 concrete architectural options with trade-offs, and (3) your recommended option.

---

## Law 3: SemVer Honesty — Zero Pre-`1.0` Compat & Zero Cruft (`review-cruft`)

Check `version` in root `Cargo.toml`. While `< 1.0.0`, **backward compatibility is NOT assumed**. The goal is the smallest possible codebase with the fewest edge cases.

### Forbidden Smells (`BLOCKER` / `MAJOR`):
- **Pre-`1.0` compatibility shims:** Keeping deprecated config keys, dual syntaxes (e.g., supporting both `modes.*` and `modes.all`, or both `::` and `.` in `${{ ... }}`), or `#[deprecated]` wrappers for unreleased/pre-1.0 ideas.
- **Dual-way / fallback execution:** Supporting two ways to configure or run the same thing ("if `slipfork.yml` missing, try `.slipforkrc`, else..."). Pick **one** canonical contract and delete everything else.
- **Incomplete cleanup of scrapped ideas:** Leaving dead types, unused enum variants, stale ADRs marked current, or leftover terminology in `docs/` and `plan/nodes/` after a pivot. When a direction changes, sweep code + tests + docs + plan nodes atomically.
- **When in doubt about breaking an already-published `0.x` user workflow:** Ask via `plan.py human add --kind compat-question` — never silently add a compatibility layer.
