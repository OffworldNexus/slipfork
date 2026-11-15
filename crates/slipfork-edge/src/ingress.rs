//! Bounded `httparse` HTTP/1.1 request dispatch and shared-host proxying into world backends.
//!
//! Requests are parsed under a `<= 16 KiB` header cap with a bounded read timeout, routed by the
//! `IngressRoute` longest-prefix table into `WorldEngine::dial_world_tcp`, and shared worlds answer
//! `308`/`421` on `*.localhost` instead of proxying.
