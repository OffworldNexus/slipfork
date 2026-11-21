//! Native S3: policy, SigV4, on-disk objects, listings, and the HTTP/1.1 server.

mod listing;
mod object_ops;
mod policy;
mod server;
mod sigv4;

pub use policy::S3Bucket;
pub use server::S3Server;
pub use sigv4::S3Credentials;
