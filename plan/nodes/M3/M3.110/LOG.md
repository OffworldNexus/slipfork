# Log M3.110

## 2026-11-24T12:58 [conductor]

created (task) Build containers run as container root in a 0->user userns; context is never mounted

## 2026-11-24T12:58 [conductor]

status ready -> ready

## 2026-11-24T13:02 [conductor]

Opus architect recommendation (adopt): per-step single mapping of the effective uid -> invoking uid (default root; USER x resolves from the rootfs /etc/passwd and runs as x inside), uid/gid size 1; Docker default caps inside the userns for root steps, empty for non-root; noNewPrivileges; delete the synthetic /etc/passwd bind; seccomp returns success for chown/fchown/lchown/fchownat (SCMP_ACT_ERRNO errnoRet 0 - verify crun accepts 0); APT_CONFIG read-only file with APT::Sandbox::User "root" for RUN only; COPY/ADD --chown resolves names against the rootfs but always writes host-user-owned; runtime pods keep the identity UID baseline; reject subordinate uids/idmapped mounts (non-user-owned store, newuidmap/subuid, kernel/fs requirements). Verify on debian:trixie-slim: apt-get install curl, useradd app, USER app + RUN id, USER postgres + initdb.

## 2026-11-24T15:07 [conductor]

status ready -> active

## 2026-11-24T15:10 [lead]

attempt 1/3 started

## 2026-11-24T15:10 [lead]

base 6192967232079691c50ee911362b35f7d08b6f79; brief: per-step single uid mapping, seccomp chown errnoRet 0 (verified on crun 1.14.1: returns success), APT_CONFIG, COPY --chown name resolution, specs/03 single-owner limits

## 2026-11-24T15:57 [lead]

Live proof: debian:trixie-slim build passed on the VM (apt-get update + install curl/postgresql, useradd app, USER app -> id=1001(app), USER postgres -> initdb PG_VERSION), every rootfs file host-uid-owned; full critical_dockerfile_live 5/5 in 208.8s. Fixed 3 latent defects the first live run exposed: ghcr /v2/ ping placeholder scope (repository:user/image:pull) made probe mint a token that ghcr 403s -> probe is now unauthenticated discovery; stale M3.108 cache-cascade expectation in the multistage journey (verified failing at baseline 6192967); external-stage COPY assertion plus the owner Dockerfile now built with the HTTPS puller (HTTP 301 dropped the bearer token).

## 2026-11-24T15:57 [lead]

status active -> review

## 2026-11-24T15:57 [lead]

status review -> done
