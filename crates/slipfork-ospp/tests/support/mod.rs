//! Test support for the protected OSPP v1 suite.
//!
//! The only stand-in is a real `127.0.0.1:0` loopback reference server speaking
//! `ospp-v1` with real P-256 verification and real RFC 9180 HPKE sealing.

pub mod server;
