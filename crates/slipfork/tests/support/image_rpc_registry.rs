//! A minimal loopback OCI Distribution v2 registry: zero-layer manifests, a tiny config blob, and
//! a tag republished only by [`Registry::publish`]. It speaks just enough HTTP for the real puller
//! and never mocks the client.

#[path = "registry.rs"]
mod http;

use std::collections::BTreeMap;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::json;

const CONFIG_DIGEST: &str =
    "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";

/// One loopback registry serving `/v2/`, manifests by tag/digest, and the config blob.
pub struct Registry {
    addr: String,
    nonce: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Registry {
    pub fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind the loopback registry");
        listener
            .set_nonblocking(true)
            .expect("the lab registry polls");
        let addr = format!(
            "127.0.0.1:{}",
            listener
                .local_addr()
                .expect("read the bound address")
                .port()
        );
        let nonce = Arc::new(AtomicUsize::new(1));
        let manifests = Arc::new(Mutex::new(BTreeMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let nonce = Arc::clone(&nonce);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            serve(&mut stream, &nonce, &manifests);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        Self {
            addr,
            nonce,
            stop,
            handle: Some(handle),
        }
    }

    /// `127.0.0.1:<port>/library/app:<tag>` for a real `ImageReference`.
    pub fn reference(&self, tag: &str) -> String {
        format!("{}/library/app:{tag}", self.addr)
    }

    /// Republishes `latest` as a new version; both manifest GETs of one pull keep seeing the
    /// previous stable digest until this call bumps the nonce.
    pub fn publish(&self) {
        self.nonce.fetch_add(1, Ordering::SeqCst);
    }

    /// The digest of the manifest the registry currently serves for `tag` (without moving it).
    pub fn manifest_digest(&self) -> String {
        http::sha256_hex(&manifest(self.nonce.load(Ordering::SeqCst)))
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve(
    stream: &mut TcpStream,
    nonce: &AtomicUsize,
    manifests: &Mutex<BTreeMap<String, Vec<u8>>>,
) {
    let Some((line, _)) = http::read_head(stream) else {
        return;
    };
    let path = line.split_whitespace().nth(1).unwrap_or_default();
    if path == "/v2/" {
        http::respond(
            stream,
            200,
            "OK",
            &[("Content-Type", "application/json".into())],
            b"{}",
        );
    } else if let Some(reference) = path.strip_prefix("/v2/library/app/manifests/") {
        if reference == "latest" {
            let body = manifest(nonce.load(Ordering::SeqCst));
            let digest = http::sha256_hex(&body);
            if let Ok(mut map) = manifests.lock() {
                map.insert(digest.clone(), body.clone());
            }
            respond_manifest(stream, &body, &digest);
        } else if let Ok(map) = manifests.lock()
            && let Some(body) = map.get(reference)
        {
            respond_manifest(stream, body, reference);
        } else {
            http::respond(stream, 404, "Not Found", &[], b"unknown manifest");
        }
    } else if path.starts_with("/v2/library/app/blobs/") {
        http::respond(
            stream,
            200,
            "OK",
            &[("Content-Type", "application/octet-stream".into())],
            b"{}",
        );
    } else {
        http::respond(stream, 404, "Not Found", &[], b"not found");
    }
}

fn respond_manifest(stream: &mut TcpStream, body: &[u8], digest: &str) {
    http::respond(
        stream,
        200,
        "OK",
        &[
            (
                "Content-Type",
                "application/vnd.oci.image.manifest.v1+json".into(),
            ),
            ("Docker-Content-Digest", digest.to_owned()),
        ],
        body,
    );
}

fn manifest(nonce: usize) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": CONFIG_DIGEST,
            "size": 2
        },
        "layers": [],
        "annotations": { "nonce": nonce.to_string() }
    }))
    .expect("encode the lab manifest")
}
