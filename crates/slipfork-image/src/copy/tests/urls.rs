//! `ADD <url>` loopback lab tests: streaming, exact destinations, and fail-closed schemes.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::MetadataExt;
use std::thread;

use crate::dockerfile::CopyOptions;
use crate::error::Error;

use super::super::{Destination, add_context, copy_context};
use super::{context, lab, owner};

/// Serves exactly one HTTP/1.1 response on a loopback ephemeral port, then closes the socket.
fn serve_once(status: u16, body: &[u8]) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind the loopback URL server");
    let port = listener
        .local_addr()
        .expect("read the loopback address")
        .port();
    let body = body.to_vec();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the loopback request");
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).expect("read the request head");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        let head = format!(
            "HTTP/1.1 {status} slipfork\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream
            .write_all(head.as_bytes())
            .expect("write the response head");
        stream.write_all(&body).expect("write the response body");
        stream.flush().expect("flush the response");
    });
    (format!("127.0.0.1:{port}"), handle)
}

/// `ADD <url> /opt/data/` streams the served bytes into the directory as a 0644 file.
#[test]
fn an_add_url_streams_into_a_directory_destination() {
    const BODY: &[u8] = b"#!/bin/sh\necho offworld\n";
    let (addr, server) = serve_once(200, BODY);
    let (context_root, context) = context(&[]);
    let rootfs = lab("rootfs-url-dir");
    let owner = owner(&rootfs);

    add_context(
        &context,
        &[format!("http://{addr}/install.sh")],
        "/opt/data/",
        Destination::new(&rootfs, "/"),
        CopyOptions::new(owner, None),
    )
    .expect("ADD http://127.0.0.1/install.sh /opt/data/");
    server.join().expect("the loopback server exits");

    let written = rootfs.join("opt/data/install.sh");
    assert_eq!(fs::read(&written).expect("read the served payload"), BODY);
    assert_eq!(
        fs::metadata(&written).expect("stat the payload").mode() & 0o777,
        0o644,
        "a URL payload lands 0644"
    );

    let _ = fs::remove_dir_all(&context_root);
    let _ = fs::remove_dir_all(&rootfs);
}

/// An exact destination writes that path, ignoring the URL's query string.
#[test]
fn an_add_url_exact_destination_writes_the_named_file() {
    const BODY: &[u8] = b"spool\n";
    let (addr, server) = serve_once(200, BODY);
    let (context_root, context) = context(&[]);
    let rootfs = lab("rootfs-url-file");
    let owner = owner(&rootfs);

    add_context(
        &context,
        &[format!("http://{addr}/download?raw=1")],
        "/usr/local/bin/install.sh",
        Destination::new(&rootfs, "/"),
        CopyOptions::new(owner, None),
    )
    .expect("ADD url to an exact file path");
    server.join().expect("the loopback server exits");

    let written = rootfs.join("usr/local/bin/install.sh");
    assert_eq!(fs::read(&written).expect("read the served payload"), BODY);
    assert_eq!(
        fs::metadata(&written).expect("stat the payload").mode() & 0o777,
        0o644
    );
    assert!(
        !rootfs.join("usr/local/bin/download").exists(),
        "an exact destination never appends the URL file name"
    );

    let _ = fs::remove_dir_all(&context_root);
    let _ = fs::remove_dir_all(&rootfs);
}

/// A non-2xx answer fails closed with the typed HTTP error and leaves no temp file behind.
#[test]
fn a_non_success_url_fails_closed_and_cleans_its_temp_file() {
    let (addr, server) = serve_once(404, b"missing\n");
    let (context_root, context) = context(&[]);
    let rootfs = lab("rootfs-url-404");
    let owner = owner(&rootfs);

    let error = add_context(
        &context,
        &[format!("http://{addr}/install.sh")],
        "/opt/data/",
        Destination::new(&rootfs, "/"),
        CopyOptions::new(owner, None),
    )
    .expect_err("a 404 must fail closed");
    server.join().expect("the loopback server exits");

    assert!(
        matches!(&error, Error::Http { status: 404, .. }),
        "a non-2xx is the typed HTTP error: {error}"
    );
    let leftovers = fs::read_dir(rootfs.join("opt/data"))
        .expect("the destination directory exists")
        .count();
    assert_eq!(leftovers, 0, "a failed fetch removes its temp file");

    let _ = fs::remove_dir_all(&context_root);
    let _ = fs::remove_dir_all(&rootfs);
}

/// Only `ADD` fetches, and only `http(s)://`; every other scheme fails closed with a remedy.
#[test]
fn only_add_fetches_http_urls() {
    let (context_root, context) = context(&[("app.conf", "k=v\n")]);
    let rootfs = lab("rootfs-url-refuse");
    let owner = owner(&rootfs);
    let destination = Destination::new(&rootfs, "/");

    let copy_error = copy_context(
        &context,
        &["http://127.0.0.1:9/x".to_owned()],
        "/opt/",
        destination,
        CopyOptions::new(owner, None),
    )
    .expect_err("COPY must never fetch a URL");
    assert!(
        copy_error.to_string().contains("use `ADD`"),
        "COPY points at ADD: {copy_error}"
    );

    let add_error = add_context(
        &context,
        &["ftp://127.0.0.1:9/x".to_owned()],
        "/opt/",
        destination,
        CopyOptions::new(owner, None),
    )
    .expect_err("only http(s) is fetchable");
    let rendered = add_error.to_string();
    assert!(
        rendered.contains("`ftp:`"),
        "the scheme is named: {rendered}"
    );
    assert!(rendered.contains("next:"), "one line plus next: {rendered}");

    let _ = fs::remove_dir_all(&context_root);
    let _ = fs::remove_dir_all(&rootfs);
}

/// One context file plus one URL into a slash-less `dest` still resolves as a directory, so
/// both sources keep their own names instead of the local copy claiming `dest` itself.
#[test]
fn a_mixed_add_puts_the_local_file_and_the_url_in_one_directory() {
    const BODY: &[u8] = b"#!/bin/sh\necho served\n";
    let (addr, server) = serve_once(200, BODY);
    let (context_root, context) = context(&[("note.txt", "local note\n")]);
    let rootfs = lab("rootfs-url-mixed");
    let owner = owner(&rootfs);

    add_context(
        &context,
        &["note.txt".to_owned(), format!("http://{addr}/install.sh")],
        "/opt",
        Destination::new(&rootfs, "/"),
        CopyOptions::new(owner, None),
    )
    .expect("ADD a context file and a URL into one directory");
    server.join().expect("the loopback server exits");

    assert_eq!(
        fs::read_to_string(rootfs.join("opt/note.txt")).expect("read the local copy"),
        "local note\n"
    );
    assert_eq!(
        fs::read(rootfs.join("opt/install.sh")).expect("read the served payload"),
        BODY
    );

    let _ = fs::remove_dir_all(&context_root);
    let _ = fs::remove_dir_all(&rootfs);
}

/// A gzipped body is an opaque URL payload: it lands byte-for-byte and is never unpacked.
#[test]
fn a_gzipped_url_payload_is_written_verbatim_never_extracted() {
    let raw = b"#!/bin/sh\necho gz\n";
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(raw).expect("write the gzip payload");
    let gz = encoder.finish().expect("finish the gzip payload");
    let (addr, server) = serve_once(200, &gz);
    let (context_root, context) = context(&[]);
    let rootfs = lab("rootfs-url-gzip");
    let owner = owner(&rootfs);

    add_context(
        &context,
        &[format!("http://{addr}/bundle.tar.gz")],
        "/opt/data/",
        Destination::new(&rootfs, "/"),
        CopyOptions::new(owner, None),
    )
    .expect("ADD a gzipped url payload");
    server.join().expect("the loopback server exits");

    assert_eq!(
        fs::read(rootfs.join("opt/data/bundle.tar.gz")).expect("read the payload"),
        gz,
        "a URL payload is never tar-extracted"
    );
    assert!(
        !rootfs.join("opt/data/bundle.tar").exists(),
        "no archive is unpacked from a URL"
    );

    let _ = fs::remove_dir_all(&context_root);
    let _ = fs::remove_dir_all(&rootfs);
}
