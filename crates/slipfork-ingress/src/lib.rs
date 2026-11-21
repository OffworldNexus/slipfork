//! `.localhost` ingress: route tables, a streaming HTTP/1.1 + WebSocket relay, and the
//! single-descriptor handoff of an accepted `:80` connection to the daemon.
//!
//! Library code here never reads environment variables, probes the process table, or binds a
//! socket: the composition root injects the accepted stream, the route table, and the upstream
//! dialer, and the daemon's ingress module makes the routing decision.

#![deny(unsafe_code)]

pub mod handoff;
pub mod proxy;
pub mod route;

pub use handoff::{ACCEPT_METHOD, IngressHandoff};
pub use proxy::{HEADER_CAP, ProxyError, RELAY_CAP, serve_connection};
pub use route::{IngressRegistry, IngressRoute, IngressTable, RouteError, RouteMatch, TableError};
pub use slipfork_world::fdpass::{PAYLOAD_CAP, recv_fd, send_fd};
