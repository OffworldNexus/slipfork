# Log M2

## 2026-11-14T22:03 [strategist]

created (milestone) snow.yml v2, OSPP v1 & Live Dev/Worktree Worlds (0.2.0)

## 2026-11-15T02:16 [strategist]

edited fields

## 2026-11-15T11:46 [strategist]

edited fields

## 2026-11-15T16:17 [conductor]

OWNER M2/M3 DIRECTIVES: (1) OSPP v1 WebSocket protocol with tpmd/SSH-style challenge-response auth, streaming DB dumps (opt-in for local dev, skipped for hermetic tests), and remote resource descriptors (e.g. shared dev S3 bucket) governed by configurable ResourceStrategy; (2) First-class hook/migration failure debugging (live output, failed world kept inspectable, manual `slipfork hook run` & `slipfork shell` & `slipfork logs`); (3) `slipforkd` runs as current user with port 80 passed via systemd socket activation (`LISTEN_FDS`, plus `--listen` flag for unprivileged tests); (4) Tunnel Weaver 64-char service label truncation (`<prefix>-<short_id>` when over limit); (5) Built-in `slipfork db`, `slipfork redis`, and `slipfork forward` commands; (6) H-009 Option B ratified; (7) Local-only external validation in ~/.cache/slipfork-ext-validation/ (strictly outside git per Confidentiality rule).

## 2026-11-15T16:29 [strategist]

edited fields
