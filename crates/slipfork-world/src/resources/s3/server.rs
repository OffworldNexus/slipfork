//! The world's native S3 HTTP/1.1 endpoint: one listener, path + virtual-host addressing.

use std::convert::Infallible;
use std::fs;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE, ETAG, HOST, HeaderValue, LAST_MODIFIED};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;

use super::listing::ListQuery;
use super::object_ops::S3BucketStore;
use super::policy::S3Bucket;
use super::sigv4::{Auth, S3Credentials, SigV4Verifier, percent_decode};
use crate::control::S3WorldConfig;
use crate::error::{Error, Result};

/// Largest object body accepted in one request.
const MAX_BODY: usize = 64 * 1024 * 1024;

/// The native S3 server for one world.
#[derive(Debug)]
pub struct S3Server {
    root: PathBuf,
    listen: SocketAddr,
    buckets: Vec<S3Bucket>,
    credentials: Vec<S3Credentials>,
}

impl S3Server {
    /// Validates every bucket, creates `<s3_root>/<bucket>/{objects,meta}`, and captures credentials.
    pub fn new(config: S3WorldConfig) -> Result<Self> {
        fs::create_dir_all(&config.s3_root)
            .map_err(|source| Error::io("create", &config.s3_root, source))?;
        let mut buckets = Vec::new();
        let mut credentials: Vec<S3Credentials> = Vec::new();
        for spec in &config.buckets {
            let bucket = S3Bucket::new(&spec.name, spec.policy)?;
            S3BucketStore::new(&config.s3_root, &bucket).create()?;
            let identity = S3Credentials::new(&spec.access_key, spec.secret_key.expose_secret());
            if !credentials
                .iter()
                .any(|known| known.access_key() == identity.access_key())
            {
                credentials.push(identity);
            }
            buckets.push(bucket);
        }
        Ok(Self {
            root: config.s3_root,
            listen: config.listen_addr,
            buckets,
            credentials,
        })
    }

    /// The address the init bound (and `Dial { port }` reaches).
    #[must_use]
    pub const fn listen_addr(&self) -> SocketAddr {
        self.listen
    }

    /// Serves HTTP/1.1 keep-alive until `shutdown` resolves.
    ///
    /// The caller owns the runtime: every connection task is spawned through the injected
    /// [`tokio::runtime::Handle`], so this library type never constructs a runtime of its own.
    ///
    /// # Errors
    ///
    /// Returns the listener accept failure that stopped the loop.
    pub async fn serve(
        self,
        handle: &tokio::runtime::Handle,
        listener: std::net::TcpListener,
        shutdown: impl Future<Output = ()> + Send,
    ) -> io::Result<()> {
        listener.set_nonblocking(true)?;
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let server = Arc::new(self);
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                accepted = listener.accept() => {
                    let (stream, _) = accepted?;
                    let server = Arc::clone(&server);
                    drop(handle.spawn(async move {
                        let service = service_fn(move |request| {
                            handle_request(Arc::clone(&server), request)
                        });
                        let _ = http1::Builder::new()
                            .keep_alive(true)
                            .serve_connection(TokioIo::new(stream), service)
                            .await;
                    }));
                }
            }
        }
        Ok(())
    }

    fn bucket(&self, name: &str) -> Option<&S3Bucket> {
        self.buckets.iter().find(|bucket| bucket.name() == name)
    }

    /// Resolves the bucket and key once: `<bucket>.s3` virtual-host, then path-style, then
    /// `<bucket>.localhost`.
    ///
    /// The ingress keeps the bucket in the path and forwards `Host: <bucket>.<world>.localhost`,
    /// so a bare leading host label must never shadow `/<bucket>/<key>`.
    fn resolve(&self, parts: &hyper::http::request::Parts) -> Option<(S3Bucket, Option<String>)> {
        let path = percent_decode(parts.uri.path());
        let host = parts
            .headers
            .get(HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let labels: Vec<&str> = host
            .split(':')
            .next()
            .unwrap_or_default()
            .split('.')
            .filter(|label| !label.is_empty())
            .collect();
        let key = || -> Option<String> {
            (!path.trim_start_matches('/').is_empty())
                .then(|| path.trim_start_matches('/').to_owned())
        };
        // `s3` marks an explicit virtual-host name: `<bucket>.s3.<world>.localhost`.
        if let Some(index) = labels.iter().position(|label| *label == "s3")
            && index > 0
            && let Some(bucket) = self.bucket(labels[index - 1])
        {
            return Some((bucket.clone(), key()));
        }
        // Path-style `/<bucket>/<key>` wins next: this is the ingress-routed shape.
        let segments: Vec<&str> = path
            .trim_start_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect();
        if let Some(name) = segments.first()
            && let Some(bucket) = self.bucket(name)
        {
            let key = segments[1..].join("/");
            return Some((bucket.clone(), (!key.is_empty()).then_some(key)));
        }
        // Last resort: `<bucket>.localhost` with the key in the path.
        let label = *labels.first()?;
        let bucket = self.bucket(label)?.clone();
        Some((bucket, key()))
    }
}

async fn handle_request(
    server: Arc<S3Server>,
    request: Request<Incoming>,
) -> std::result::Result<Response<Full<Bytes>>, Infallible> {
    let head = request.method() == Method::HEAD;
    let response = route(server, request).await;
    Ok(if head {
        without_body(response)
    } else {
        response
    })
}

/// `HEAD` responses never carry a body and never advertise one.
fn without_body(response: Response<Full<Bytes>>) -> Response<Full<Bytes>> {
    let (mut parts, _body) = response.into_parts();
    if !parts.status.is_success() {
        parts
            .headers
            .insert(CONTENT_LENGTH, HeaderValue::from_static("0"));
    }
    Response::from_parts(parts, Full::new(Bytes::new()))
}

async fn route(server: Arc<S3Server>, request: Request<Incoming>) -> Response<Full<Bytes>> {
    let (parts, body) = request.into_parts();
    let method = parts.method.clone();
    let query = parts.uri.query().unwrap_or_default().to_owned();
    let Some((bucket, key)) = server.resolve(&parts) else {
        return error(StatusCode::NOT_FOUND, "NoSuchBucket");
    };
    let store = S3BucketStore::new(&server.root, &bucket);
    let listing = method == Method::GET && query.contains("list-type=2");
    let auth = SigV4Verifier::new(&server.credentials).verify(
        method.as_str(),
        parts.uri.path(),
        &query,
        &parts.headers,
    );
    match auth {
        Auth::Invalid => return error(StatusCode::FORBIDDEN, "AccessDenied"),
        Auth::Anonymous if listing || !bucket.permits_anonymous(method.as_str()) => {
            return error(StatusCode::FORBIDDEN, "AccessDenied");
        }
        Auth::Anonymous | Auth::Valid => {}
    }

    let payload = if method == Method::PUT {
        match Limited::new(body, MAX_BODY).collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE, "EntityTooLarge"),
        }
    } else {
        Bytes::new()
    };

    let Some(key) = key else {
        if listing {
            return list_xml(&store, &query)
                .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "InternalError"));
        }
        return match method {
            Method::GET | Method::HEAD => empty(StatusCode::OK),
            _ => error(StatusCode::METHOD_NOT_ALLOWED, "MethodNotAllowed"),
        };
    };
    let result = match method {
        Method::PUT => put(&store, &key, &parts, &payload),
        Method::GET => get(&store, &key, false),
        Method::HEAD => get(&store, &key, true),
        Method::DELETE => delete(&store, &key),
        _ => Ok(error(StatusCode::METHOD_NOT_ALLOWED, "MethodNotAllowed")),
    };
    result.unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "InternalError"))
}

fn put(
    store: &S3BucketStore,
    key: &str,
    parts: &hyper::http::request::Parts,
    body: &[u8],
) -> crate::Result<Response<Full<Bytes>>> {
    let content_type = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream");
    let meta = store.put(key, content_type, body)?;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(ETAG, meta.etag)
        .body(Full::new(Bytes::new()))
        .expect("valid PUT response"))
}

fn get(store: &S3BucketStore, key: &str, head: bool) -> crate::Result<Response<Full<Bytes>>> {
    let Some((meta, body)) = store.get(key)? else {
        return Ok(error(StatusCode::NOT_FOUND, "NoSuchKey"));
    };
    let body = if head {
        Bytes::new()
    } else {
        Bytes::from(body)
    };
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, meta.content_type)
        .header(CONTENT_LENGTH, meta.content_length)
        .header(ETAG, meta.etag)
        .header(LAST_MODIFIED, meta.last_modified)
        .body(Full::new(body))
        .expect("valid GET response"))
}

fn delete(store: &S3BucketStore, key: &str) -> crate::Result<Response<Full<Bytes>>> {
    store.delete(key)?;
    Ok(empty(StatusCode::NO_CONTENT))
}

fn list_xml(store: &S3BucketStore, query: &str) -> crate::Result<Response<Full<Bytes>>> {
    let query = parse_query(query);
    let listing = store.list(&query)?;
    Ok(xml(StatusCode::OK, listing.xml()))
}

fn parse_query(query: &str) -> ListQuery {
    let mut parsed = ListQuery {
        prefix: String::new(),
        delimiter: None,
        max_keys: ListQuery::DEFAULT_MAX_KEYS,
        continuation_token: None,
    };
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = percent_decode(&value.replace('+', " "));
        match name {
            "prefix" => parsed.prefix = value,
            "delimiter" => parsed.delimiter = (!value.is_empty()).then_some(value),
            "max-keys" => parsed.max_keys = value.parse().unwrap_or(ListQuery::DEFAULT_MAX_KEYS),
            "continuation-token" => {
                parsed.continuation_token = (!value.is_empty()).then_some(value)
            }
            _ => {}
        }
    }
    parsed
}

fn empty(status: StatusCode) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(CONTENT_LENGTH, 0)
        .body(Full::new(Bytes::new()))
        .expect("valid empty response")
}

fn xml(status: StatusCode, body: String) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/xml")
        .header(CONTENT_LENGTH, body.len())
        .body(Full::new(Bytes::from(body)))
        .expect("valid XML response")
}

/// The canonical user-facing message for one S3 `Error` code.
fn error_message(code: &str) -> &'static str {
    match code {
        "AccessDenied" => "Access Denied",
        "NoSuchKey" => "The specified key does not exist.",
        "NoSuchBucket" => "The specified bucket does not exist.",
        "EntityTooLarge" => "Your proposed upload exceeds the maximum allowed object size.",
        "MethodNotAllowed" => "The specified method is not allowed against this resource.",
        "InternalError" => "We encountered an internal error. Please try again.",
        _ => "",
    }
}

fn error(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    xml(
        status,
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <Error><Code>{code}</Code><Message>{}</Message></Error>",
            error_message(code)
        ),
    )
}
