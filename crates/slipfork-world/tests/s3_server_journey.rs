//! Lab journey for the native S3 endpoint: one real loopback listener, one real on-disk store.
//!
//! Every request is a hand-written HTTP/1.1 exchange over `127.0.0.1:0`; every object lands on a
//! real `tempfile` directory under `/var/tmp`. No mocks, no fake clock, no fake bucket shim.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use slipfork_forkfile::BucketPolicy;
use slipfork_world::{S3BucketSpec, S3Server, S3WorldConfig};

const ACCESS: &str = "slipfork";
const SECRET: &str = "slipfork-demo-s3";
const REGION: &str = "us-east-1";
const AMZ_DATE: &str = "20260101T000000Z";
const DATE: &str = "20260101";
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const HELLO_MD5: &str = "\"5d41402abc4b2a76b9719d911017c592\"";

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }
}

/// One running server plus the runtime that owns it; dropping it stops the accept loop.
struct Rig {
    addr: SocketAddr,
    runtime: tokio::runtime::Runtime,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Rig {
    fn start(store: &Path, buckets: Vec<S3BucketSpec>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local address");
        let server = S3Server::new(S3WorldConfig {
            listen_addr: addr,
            s3_root: store.to_path_buf(),
            buckets,
        })
        .expect("the native server validates its buckets");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("test runtime");
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        let handle = runtime.handle().clone();
        runtime.spawn(async move {
            let _ = server
                .serve(&handle, listener, async move {
                    let _ = stopped.await;
                })
                .await;
        });
        Self {
            addr,
            runtime,
            shutdown: Some(shutdown),
        }
    }

    fn host(&self) -> String {
        format!("127.0.0.1:{}", self.addr.port())
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.runtime.block_on(async {
            tokio::time::sleep(Duration::from_millis(50)).await;
        });
    }
}

fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    host: &str,
    extra: &[(&str, String)],
    body: &[u8],
) -> Reply {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .unwrap_or_else(|error| panic!("connect {addr}: {error}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    let mut raw = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in extra {
        raw.push_str(&format!("{name}: {value}\r\n"));
    }
    raw.push_str("\r\n");
    stream
        .write_all(raw.as_bytes())
        .expect("write request head");
    stream.write_all(body).expect("write request body");
    read_reply(&mut stream, method != "HEAD")
}

fn read_reply(stream: &mut TcpStream, has_body: bool) -> Reply {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    while split_headers(&buffer).is_none() {
        let read = stream.read(&mut chunk).expect("read response head");
        assert!(read > 0, "peer closed before the response head");
        buffer.extend_from_slice(&chunk[..read]);
    }
    let split = split_headers(&buffer).expect("header terminator");
    let head = String::from_utf8(buffer[..split].to_vec()).expect("UTF-8 head");
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("status line");
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let mut body = buffer[split + 4..].to_vec();
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .map_or(0, |(_, value)| value.parse::<usize>().expect("length"));
    let wanted = if has_body { length } else { 0 };
    while body.len() < wanted {
        let read = stream.read(&mut chunk).expect("read response body");
        assert!(read > 0, "peer closed before the response body");
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(wanted);
    Reply {
        status,
        headers,
        body,
    }
}

fn split_headers(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

/// Signs one request exactly like an AWS SDK: header auth over the given payload digest.
fn signed_with(
    method: &str,
    path: &str,
    query: &str,
    host: &str,
    payload: &str,
) -> Vec<(&'static str, String)> {
    let headers = format!("host:{host}\nx-amz-content-sha256:{payload}\nx-amz-date:{AMZ_DATE}\n");
    let signed = "host;x-amz-content-sha256;x-amz-date";
    let canonical = format!("{method}\n{path}\n{query}\n{headers}\n{signed}\n{payload}");
    let scope = format!("{DATE}/{REGION}/s3/aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{AMZ_DATE}\n{scope}\n{}",
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let key = hmac(
        &hmac(
            &hmac(&hmac(format!("AWS4{SECRET}").as_bytes(), DATE), REGION),
            "s3",
        ),
        "aws4_request",
    );
    let signature = hex(&hmac(&key, &to_sign));
    vec![
        ("x-amz-date", AMZ_DATE.to_owned()),
        ("x-amz-content-sha256", payload.to_owned()),
        (
            "Authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={ACCESS}/{scope}, SignedHeaders={signed}, Signature={signature}"
            ),
        ),
    ]
}

fn signed(
    method: &str,
    path: &str,
    query: &str,
    host: &str,
    body: &[u8],
) -> Vec<(&'static str, String)> {
    signed_with(method, path, query, host, &hex(&Sha256::digest(body)))
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC key");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn spec(name: &str, policy: BucketPolicy) -> S3BucketSpec {
    S3BucketSpec {
        name: name.to_owned(),
        policy,
        access_key: ACCESS.to_owned(),
        secret_key: slipfork_forkfile::SecretValue::new(SECRET),
    }
}

fn store_is_real(store: &Path, bucket: &str, key: &str, body: &[u8]) {
    let object = store.join(bucket).join("objects").join(key);
    assert_eq!(
        fs::read(&object).expect("object bytes on disk"),
        body,
        "the endpoint must write the payload to `{}`",
        object.display()
    );
    let sidecar = store.join(bucket).join("meta").join(format!("{key}.json"));
    assert!(
        sidecar.is_file(),
        "the endpoint must write the metadata sidecar `{}`",
        sidecar.display()
    );
}

#[test]
fn native_s3_serves_real_objects_with_sigv4_and_bucket_policy() {
    let temp = tempfile::Builder::new()
        .prefix("slipfork-s3-journey-")
        .tempdir_in("/var/tmp")
        .expect("the lab needs real disk under /var/tmp");
    let store = temp.path().join("s3");
    let rig = Rig::start(
        &store,
        vec![
            spec("uploads", BucketPolicy::Private),
            spec("assets", BucketPolicy::PublicRead),
        ],
    );
    let addr = rig.addr;
    let host = rig.host();

    // Private bucket: a valid signature writes and reads; anonymous access is refused.
    let put = request(
        addr,
        "PUT",
        "/uploads/hello.txt",
        &host,
        &signed("PUT", "/uploads/hello.txt", "", &host, b"hello"),
        b"hello",
    );
    assert_eq!(put.status, 200, "signed PUT must succeed");
    assert_eq!(put.header("etag"), Some(HELLO_MD5), "MD5 ETag");
    store_is_real(&store, "uploads", "hello.txt", b"hello");

    // Nested keys create their sidecar parent directory and round-trip through GET, HEAD, list.
    let nested_put = request(
        addr,
        "PUT",
        "/uploads/nested/deep.txt",
        &host,
        &signed("PUT", "/uploads/nested/deep.txt", "", &host, b"nested"),
        b"nested",
    );
    assert_eq!(nested_put.status, 200, "nested key PUT");
    store_is_real(&store, "uploads", "nested/deep.txt", b"nested");
    let nested_get = request(
        addr,
        "GET",
        "/uploads/nested/deep.txt",
        &host,
        &signed("GET", "/uploads/nested/deep.txt", "", &host, b""),
        b"",
    );
    assert_eq!(nested_get.status, 200);
    assert_eq!(nested_get.body, b"nested");
    let nested_head = request(
        addr,
        "HEAD",
        "/uploads/nested/deep.txt",
        &host,
        &signed("HEAD", "/uploads/nested/deep.txt", "", &host, b""),
        b"",
    );
    assert_eq!(nested_head.status, 200);
    assert!(nested_head.body.is_empty(), "HEAD must not carry a body");
    assert_eq!(nested_head.header("content-length"), Some("6"));
    let nested_list = request(
        addr,
        "GET",
        "/uploads?list-type=2&prefix=nested",
        &host,
        &signed("GET", "/uploads", "list-type=2&prefix=nested", &host, b""),
        b"",
    );
    assert_eq!(nested_list.status, 200);
    assert!(
        String::from_utf8(nested_list.body)
            .expect("XML")
            .contains("<Key>nested/deep.txt</Key>")
    );

    let get = request(
        addr,
        "GET",
        "/uploads/hello.txt",
        &host,
        &signed("GET", "/uploads/hello.txt", "", &host, b""),
        b"",
    );
    assert_eq!(get.status, 200);
    assert_eq!(get.body, b"hello");
    assert_eq!(get.header("content-type"), Some("application/octet-stream"));
    assert_eq!(get.header("content-length"), Some("5"));

    let head = request(
        addr,
        "HEAD",
        "/uploads/hello.txt",
        &host,
        &signed("HEAD", "/uploads/hello.txt", "", &host, b""),
        b"",
    );
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty(), "HEAD must not carry a body");
    assert_eq!(head.header("etag"), Some(HELLO_MD5));
    assert_eq!(head.header("content-length"), Some("5"));

    let anonymous = request(addr, "GET", "/uploads/hello.txt", &host, &[], b"");
    assert_eq!(
        anonymous.status, 403,
        "private buckets refuse anonymous GET"
    );
    let anon_priv_head = request(addr, "HEAD", "/uploads/hello.txt", &host, &[], b"");
    assert_eq!(
        anon_priv_head.status, 403,
        "private buckets refuse anonymous HEAD"
    );
    let anon_priv_put = request(addr, "PUT", "/uploads/intruder.txt", &host, &[], b"x");
    assert_eq!(
        anon_priv_put.status, 403,
        "private buckets refuse anonymous PUT"
    );
    let anon_priv_del = request(addr, "DELETE", "/uploads/hello.txt", &host, &[], b"");
    assert_eq!(
        anon_priv_del.status, 403,
        "private buckets refuse anonymous DELETE"
    );

    let mut tampered = signed("GET", "/uploads/hello.txt", "", &host, b"");
    if let Some((_, value)) = tampered
        .iter_mut()
        .find(|(name, _)| *name == "Authorization")
    {
        value.push('0');
    }
    let forged = request(addr, "GET", "/uploads/hello.txt", &host, &tampered, b"");
    assert_eq!(forged.status, 403, "a wrong signature is refused");

    // Virtual-host addressing: `<bucket>.s3.<world>.localhost` with the key in the path.
    let vhost = "uploads.s3.demo.localhost";
    let virtual_get = request(
        addr,
        "GET",
        "/hello.txt",
        vhost,
        &signed("GET", "/hello.txt", "", vhost, b""),
        b"",
    );
    assert_eq!(virtual_get.status, 200, "vhost GET resolves the bucket");
    assert_eq!(virtual_get.body, b"hello");

    // Ingress traffic rewrites the path to `/<bucket>/<key>` but forwards `Host: <bucket>.<world>`.
    let ingress_put = request(
        addr,
        "PUT",
        "/assets/hello.txt",
        "assets.localhost",
        &signed("PUT", "/assets/hello.txt", "", "assets.localhost", b"hello"),
        b"hello",
    );
    assert_eq!(
        ingress_put.status, 200,
        "path-style PUT via the ingress host"
    );
    store_is_real(&store, "assets", "hello.txt", b"hello");
    let ingress_get = request(
        addr,
        "GET",
        "/assets/hello.txt",
        "assets.localhost",
        &[],
        b"",
    );
    assert_eq!(
        ingress_get.status, 200,
        "the ingress host must resolve path-style first"
    );
    assert_eq!(
        ingress_get.body, b"hello",
        "`/assets/hello.txt` must not become bucket `assets` key `assets/hello.txt`"
    );

    // An explicit `s3` label still selects virtual-host addressing with the key in the path.
    let tagged_vhost = "assets.s3.localhost";
    let tagged_get = request(addr, "GET", "/hello.txt", tagged_vhost, &[], b"");
    assert_eq!(
        tagged_get.status, 200,
        "`<bucket>.s3.*` resolves virtual-host style"
    );
    assert_eq!(tagged_get.body, b"hello");

    // ListObjectsV2: anonymous listing is refused, signed listing returns the key.
    let anon_list = request(addr, "GET", "/uploads?list-type=2", &host, &[], b"");
    assert_eq!(anon_list.status, 403, "anonymous list is refused");
    let list = request(
        addr,
        "GET",
        "/uploads?list-type=2&prefix=he",
        &host,
        &signed("GET", "/uploads", "list-type=2&prefix=he", &host, b""),
        b"",
    );
    assert_eq!(list.status, 200);
    let listing = String::from_utf8(list.body).expect("XML listing");
    assert!(listing.contains("<Name>uploads</Name>"), "{listing}");
    assert!(listing.contains("<Key>hello.txt</Key>"), "{listing}");
    assert!(
        listing.contains("<IsTruncated>false</IsTruncated>"),
        "{listing}"
    );

    // `max-keys` is echoed, pages truncate, and the token resumes after the last key.
    let page_one = request(
        addr,
        "GET",
        "/uploads?list-type=2&max-keys=1",
        &host,
        &signed("GET", "/uploads", "list-type=2&max-keys=1", &host, b""),
        b"",
    );
    assert_eq!(page_one.status, 200);
    let page_one = String::from_utf8(page_one.body).expect("XML page");
    assert!(page_one.contains("<MaxKeys>1</MaxKeys>"), "{page_one}");
    assert!(
        page_one.contains("<IsTruncated>true</IsTruncated>"),
        "{page_one}"
    );
    assert!(
        page_one.contains("<NextContinuationToken>hello.txt</NextContinuationToken>"),
        "{page_one}"
    );
    assert!(!page_one.contains("nested/deep.txt"), "{page_one}");
    let page_two = request(
        addr,
        "GET",
        "/uploads?list-type=2&max-keys=1&continuation-token=hello.txt",
        &host,
        &signed(
            "GET",
            "/uploads",
            "continuation-token=hello.txt&list-type=2&max-keys=1",
            &host,
            b"",
        ),
        b"",
    );
    assert_eq!(page_two.status, 200);
    let page_two = String::from_utf8(page_two.body).expect("XML page");
    assert!(
        page_two.contains("<Key>nested/deep.txt</Key>"),
        "{page_two}"
    );
    assert!(
        page_two.contains("<IsTruncated>false</IsTruncated>"),
        "{page_two}"
    );

    // A delimiter collapses the nested key into `CommonPrefixes`.
    let delimited = request(
        addr,
        "GET",
        "/uploads?list-type=2&delimiter=/",
        &host,
        &signed("GET", "/uploads", "delimiter=%2F&list-type=2", &host, b""),
        b"",
    );
    assert_eq!(delimited.status, 200);
    let delimited = String::from_utf8(delimited.body).expect("XML listing");
    assert!(delimited.contains("<Key>hello.txt</Key>"), "{delimited}");
    assert!(
        delimited.contains("<CommonPrefixes><Prefix>nested/</Prefix></CommonPrefixes>"),
        "{delimited}"
    );

    // Public-read bucket: anonymous reads work, anonymous writes and listings stay closed.
    let public_put = request(
        addr,
        "PUT",
        "/assets/logo.png",
        &host,
        &signed("PUT", "/assets/logo.png", "", &host, b"png"),
        b"png",
    );
    assert_eq!(public_put.status, 200, "signed PUT on a public bucket");
    store_is_real(&store, "assets", "logo.png", b"png");
    let public_get = request(addr, "GET", "/assets/logo.png", &host, &[], b"");
    assert_eq!(public_get.status, 200, "public-read serves anonymous GET");
    assert_eq!(public_get.body, b"png");
    let public_head = request(addr, "HEAD", "/assets/logo.png", &host, &[], b"");
    assert_eq!(public_head.status, 200, "public-read serves anonymous HEAD");
    assert!(public_head.body.is_empty());
    let signed_public_list = request(
        addr,
        "GET",
        "/assets?list-type=2",
        &host,
        &signed("GET", "/assets", "list-type=2", &host, b""),
        b"",
    );
    assert_eq!(
        signed_public_list.status, 200,
        "signed ListObjectsV2 on public-read succeeds"
    );
    let anon_put = request(addr, "PUT", "/assets/other.txt", &host, &[], b"nope");
    assert_eq!(anon_put.status, 403, "anonymous PUT is refused");
    let anon_delete = request(addr, "DELETE", "/assets/logo.png", &host, &[], b"");
    assert_eq!(anon_delete.status, 403, "anonymous DELETE is refused");
    let public_list = request(addr, "GET", "/assets?list-type=2", &host, &[], b"");
    assert_eq!(public_list.status, 403, "anonymous list is refused");
    assert!(
        !store.join("assets/objects/other.txt").exists(),
        "a refused PUT must leave no object behind"
    );
    let signed_public_del = request(
        addr,
        "DELETE",
        "/assets/logo.png",
        &host,
        &signed("DELETE", "/assets/logo.png", "", &host, b""),
        b"",
    );
    assert_eq!(
        signed_public_del.status, 204,
        "signed DELETE on public-read succeeds"
    );

    // Signed DELETE removes the object and its sidecar; the next read is 404 with S3's message.
    let delete = request(
        addr,
        "DELETE",
        "/uploads/hello.txt",
        &host,
        &signed("DELETE", "/uploads/hello.txt", "", &host, b""),
        b"",
    );
    assert_eq!(delete.status, 204);
    assert!(!store.join("uploads/objects/hello.txt").exists());
    assert!(!store.join("uploads/meta/hello.txt.json").exists());
    let gone = request(
        addr,
        "GET",
        "/uploads/hello.txt",
        &host,
        &signed("GET", "/uploads/hello.txt", "", &host, b""),
        b"",
    );
    assert_eq!(gone.status, 404, "a deleted key is NoSuchKey");
    assert_eq!(gone.header("content-type"), Some("application/xml"));
    let gone_body = String::from_utf8(gone.body).expect("XML error");
    assert!(
        gone_body.contains("<Message>The specified key does not exist.</Message>"),
        "{gone_body}"
    );
    let gone_head = request(
        addr,
        "HEAD",
        "/uploads/hello.txt",
        &host,
        &signed("HEAD", "/uploads/hello.txt", "", &host, b""),
        b"",
    );
    assert_eq!(gone_head.status, 404);
    assert!(gone_head.body.is_empty(), "HEAD errors carry no body");

    // A listing that cannot read `objects/` is a hard 500, never a fabricated empty bucket.
    fs::remove_dir_all(store.join("uploads/objects")).expect("remove objects dir");
    fs::write(store.join("uploads/objects"), b"not a directory").expect("replace with a file");
    let broken = request(
        addr,
        "GET",
        "/uploads?list-type=2",
        &host,
        &signed("GET", "/uploads", "list-type=2", &host, b""),
        b"",
    );
    assert_eq!(broken.status, 500, "a listing IO failure is internal");
    let broken_body = String::from_utf8(broken.body).expect("XML error");
    assert!(
        broken_body.contains("<Code>InternalError</Code>"),
        "{broken_body}"
    );
}

#[test]
fn unsigned_payload_reads_and_sorted_canonical_queries_are_accepted() {
    let temp = tempfile::Builder::new()
        .prefix("slipfork-s3-unsigned-")
        .tempdir_in("/var/tmp")
        .expect("the lab needs real disk under /var/tmp");
    let store = temp.path().join("s3");
    let rig = Rig::start(&store, vec![spec("uploads", BucketPolicy::Private)]);
    let addr = rig.addr;
    let host = rig.host();

    let put = request(
        addr,
        "PUT",
        "/uploads/hello.txt",
        &host,
        &signed_with(
            "PUT",
            "/uploads/hello.txt",
            "",
            &host,
            &hex(&Sha256::digest(b"hello")),
        ),
        b"hello",
    );
    assert_eq!(put.status, 200);

    // `UNSIGNED-PAYLOAD` is a legitimate read-only signature for GET/HEAD.
    let unsigned = request(
        addr,
        "GET",
        "/uploads/hello.txt",
        &host,
        &signed_with("GET", "/uploads/hello.txt", "", &host, "UNSIGNED-PAYLOAD"),
        b"",
    );
    assert_eq!(unsigned.status, 200, "UNSIGNED-PAYLOAD GET is accepted");
    assert_eq!(unsigned.body, b"hello");

    // The wire query is `?b=2&a=1`; canonicalization sorts it to `a=1&b=2` before verifying.
    let sorted = request(
        addr,
        "GET",
        "/uploads/hello.txt?b=2&a=1",
        &host,
        &signed_with("GET", "/uploads/hello.txt", "a=1&b=2", &host, EMPTY_SHA256),
        b"",
    );
    assert_eq!(sorted.status, 200, "query parameters canonicalize sorted");
    assert_eq!(sorted.body, b"hello");
}
