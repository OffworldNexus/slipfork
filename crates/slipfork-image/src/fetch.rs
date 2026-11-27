//! Remote `ADD <http(s)://…>` sources: one streamed GET into the writable step snapshot.
//!
//! Downloads reuse the registry's shared timeout and TLS configuration
//! ([`agent`](crate::registry::agent)), so `ADD` and pulls behave the same on slow or dead hosts.
//! The body is never buffered in memory: bytes flow into a temp file beside the destination and
//! are renamed into place only after the whole body arrived under the size cap.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::{PermissionsExt, chown};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use percent_encoding::percent_decode_str;

use crate::dockerfile::CopyOptions;
use crate::error::{Error, Result};
use crate::registry;

/// Hard cap on one `ADD <url>` download (2 GiB) so a runaway or hostile server cannot fill the
/// step snapshot. Bytes are streamed and counted, never buffered whole.
pub(crate) const MAX_URL_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Read size for the streamed download.
const CHUNK: usize = 64 * 1024;

/// Bytes in a gibibyte, for a human cap label.
const GIB: u64 = 1024 * 1024 * 1024;

/// The remote scheme `source` names, if it is a URL rather than a context path.
///
/// Only a scheme followed by `//` (or the literal `file:` scheme) counts, so a context file whose
/// name merely contains a colon is still treated as a path.
pub(crate) fn scheme(source: &str) -> Option<&str> {
    let (scheme, rest) = source.split_once(':')?;
    if !scheme.starts_with(|first: char| first.is_ascii_alphabetic())
        || !scheme.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
    {
        return None;
    }
    if rest.starts_with("//") || scheme.eq_ignore_ascii_case("file") {
        Some(scheme)
    } else {
        None
    }
}

/// Whether `source` is an `http://` or `https://` URL the builder may fetch.
pub(crate) fn is_http(source: &str) -> bool {
    scheme(source).is_some_and(|scheme| {
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
    })
}

/// The file name a URL contributes when it lands in a directory destination.
///
/// The last path segment (query and fragment stripped) is percent-decoded once; an empty, `.`,
/// `..`, or separator-bearing segment fails closed so a URL can never name a path outside the
/// resolved destination.
pub(crate) fn remote_name(url: &str) -> Result<String> {
    // Drop `scheme://host`, then the query/fragment; only the path can name the file.
    let after_authority = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest)
        .split_once('/')
        .map_or("", |(_, path)| path);
    let path = after_authority.split(['?', '#']).next().unwrap_or_default();
    let segment = path.rsplit('/').next().unwrap_or_default();
    let decoded = percent_decode_str(segment).decode_utf8().map_err(|_| {
        step(
            format!("`ADD {url}` has a file name that is not valid UTF-8"),
            "use a URL whose last path segment is a plain file name",
        )
    })?;
    if decoded.is_empty()
        || decoded == "."
        || decoded == ".."
        || decoded.contains('/')
        || decoded.contains('\\')
    {
        return Err(step(
            format!("`ADD {url}` has no usable file name"),
            "end the URL with a file name, or name the exact destination path",
        ));
    }
    Ok(decoded.into_owned())
}

/// Streams `url` into `target`, a path inside the writable step rootfs, then renames it into place
/// with the step's owner and `--chmod` mode (default `0644`). A temp file never survives a failed
/// transfer.
pub(crate) fn fetch(url: &str, target: &Path, options: CopyOptions) -> Result<()> {
    fetch_with_limit(url, target, options, MAX_URL_BYTES)
}

/// `fetch` with an injected cap, so a test can trip it with a handful of bytes.
fn fetch_with_limit(url: &str, target: &Path, options: CopyOptions, max: u64) -> Result<()> {
    let parent = target.parent().unwrap_or(Path::new("/"));
    fs::create_dir_all(parent).map_err(|source| Error::io("create", parent, source))?;
    let temp = open_temp(parent, target)?;
    if let Err(error) = download(url, &temp, max) {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    if let Err(source) = fs::rename(&temp, target) {
        let _ = fs::remove_file(&temp);
        return Err(Error::io("rename", target, source));
    }
    chown(target, Some(options.owner.uid), Some(options.owner.gid))
        .map_err(|source| Error::io("chown", target, source))?;
    let mode = options.mode.unwrap_or(0o644) & 0o7777;
    fs::set_permissions(target, fs::Permissions::from_mode(mode))
        .map_err(|source| Error::io("chmod", target, source))
}

/// Creates a collision-safe hidden temp file beside `target`.
///
/// `create_new` makes a concurrent builder's attempt fail rather than truncate its download; the
/// name mixes pid, nanoseconds, and a retry counter so stale files from a crashed build never
/// collide with this one.
fn open_temp(parent: &Path, target: &Path) -> Result<PathBuf> {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    for attempt in 0..16 {
        let path = parent.join(format!(
            ".{name}.slipfork-add-{}-{nanos}-{attempt}",
            std::process::id()
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                drop(file);
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(Error::io("create", &path, error)),
        }
    }
    Err(Error::io(
        "create",
        parent,
        io::Error::new(io::ErrorKind::AlreadyExists, "no free temp file name"),
    ))
}

/// One GET, streamed into `temp`; a non-2xx is the typed [`Error::Http`], a broken connection the
/// typed [`Error::Transport`], and more than `max` bytes fails closed mid-stream.
fn download(url: &str, temp: &Path, max: u64) -> Result<()> {
    let response = registry::agent()
        .get(url)
        .call()
        .map_err(|error| transfer(url, error))?;
    let mut body = response.into_reader();
    let mut file = File::create(temp).map_err(|source| Error::io("create", temp, source))?;
    let mut buffer = vec![0u8; CHUNK];
    let mut written: u64 = 0;
    loop {
        let read = body.read(&mut buffer).map_err(|source| Error::Transport {
            url: url.to_owned(),
            message: source.to_string(),
        })?;
        if read == 0 {
            break;
        }
        written = written.saturating_add(read as u64);
        if written > max {
            return Err(step(
                format!("`ADD {url}` exceeds the {} download cap", cap_label(max)),
                "vendor a smaller artifact into the build context or fetch it in a `RUN` step",
            ));
        }
        file.write_all(&buffer[..read])
            .map_err(|source| Error::io("write", temp, source))?;
    }
    file.flush()
        .map_err(|source| Error::io("write", temp, source))?;
    fs::set_permissions(temp, fs::Permissions::from_mode(0o644))
        .map_err(|source| Error::io("chmod", temp, source))
}

/// `2 GiB` for the shipping cap, `8 bytes` for an injected test cap.
fn cap_label(max: u64) -> String {
    if max >= GIB && max % GIB == 0 {
        format!("{} GiB", max / GIB)
    } else {
        format!("{max} bytes")
    }
}

fn transfer(url: &str, error: ureq::Error) -> Error {
    match error {
        ureq::Error::Status(status, _) => Error::Http {
            url: url.to_owned(),
            status,
        },
        ureq::Error::Transport(transport) => Error::Transport {
            url: url.to_owned(),
            message: transport.to_string(),
        },
    }
}

fn step(message: String, next: &str) -> Error {
    Error::StepFailed {
        line: 0,
        phase: "add",
        instruction: String::new().into(),
        message,
        exit_code: None,
        runtime: None,
        next: next.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::dockerfile::{CopyOptions, Owner};
    use crate::error::Error;

    use super::{fetch, fetch_with_limit, remote_name, scheme};

    fn lab(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let root = PathBuf::from("/var/tmp").join(format!(
            "slipfork-fetch-{prefix}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("the lab needs real disk under /var/tmp");
        root
    }

    fn owner(root: &std::path::Path) -> Owner {
        let metadata = fs::metadata(root).expect("stat the lab root");
        Owner {
            uid: metadata.uid(),
            gid: metadata.gid(),
        }
    }

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

    #[test]
    fn only_url_schemes_are_classified_as_remote() {
        assert_eq!(scheme("https://h/a"), Some("https"));
        assert_eq!(scheme("HTTP://h/a"), Some("HTTP"));
        assert_eq!(scheme("ftp://h/a"), Some("ftp"));
        assert_eq!(scheme("file:/etc/passwd"), Some("file"));
        assert_eq!(scheme("C:foo"), None, "a drive-ish name is a path");
        assert_eq!(scheme("note.txt"), None);
        assert_eq!(scheme("./dir/name:1"), None);
    }

    #[test]
    fn a_url_name_is_the_last_decoded_path_segment() {
        assert_eq!(
            remote_name("https://h/a/b.txt?x=1#y").expect("last segment"),
            "b.txt"
        );
        assert_eq!(
            remote_name("https://h/a%20b.txt").expect("decoded segment"),
            "a b.txt"
        );
        for url in [
            "https://h/",
            "https://h",
            "https://h/a/.",
            "https://h/a/..",
            "https://h/a/%2E%2E",
            "https://h/a/%2F",
            "https://h/a/%FF",
        ] {
            assert!(remote_name(url).is_err(), "{url} must fail closed");
        }
    }

    /// A downloaded file takes the step's `--chmod` mode and stays `0644` without one.
    #[test]
    fn a_downloaded_file_takes_the_step_mode_and_defaults_to_0644() {
        const BODY: &[u8] = b"#!/bin/sh\necho mode\n";
        let rootfs = lab("mode");
        let owner = owner(&rootfs);

        let (addr, server) = serve_once(200, BODY);
        let script = rootfs.join("opt/bin/install.sh");
        fetch(
            &format!("http://{addr}/install.sh"),
            &script,
            CopyOptions::new(owner, Some(0o755)),
        )
        .expect("a 200 response lands the payload");
        server.join().expect("the loopback server exits");
        assert_eq!(
            fs::metadata(&script)
                .expect("stat the chmodded payload")
                .mode()
                & 0o777,
            0o755,
            "`ADD --chmod=755` reaches the downloaded file"
        );

        let (addr, server) = serve_once(200, BODY);
        let plain = rootfs.join("opt/data/payload.bin");
        fetch(
            &format!("http://{addr}/payload.bin"),
            &plain,
            CopyOptions::new(owner, None),
        )
        .expect("a default fetch lands the payload");
        server.join().expect("the loopback server exits");
        assert_eq!(
            fs::metadata(&plain)
                .expect("stat the default payload")
                .mode()
                & 0o777,
            0o644,
            "without `--chmod` a download stays 0644"
        );

        let _ = fs::remove_dir_all(&rootfs);
    }

    /// Nine served bytes against an eight-byte cap: one-line `StepFailed`, no temp or target file.
    #[test]
    fn a_body_over_the_cap_fails_closed_and_leaves_no_temp_file() {
        let (addr, server) = serve_once(200, b"123456789");
        let rootfs = lab("cap");
        let owner = owner(&rootfs);
        let target = rootfs.join("opt/data/payload.bin");

        let error = fetch_with_limit(
            &format!("http://{addr}/payload.bin"),
            &target,
            CopyOptions::new(owner, None),
            8,
        )
        .expect_err("9 bytes must trip an 8-byte cap");
        server.join().expect("the loopback server exits");

        let rendered = error.to_string();
        assert!(matches!(&error, Error::StepFailed { .. }), "{rendered}");
        assert!(rendered.contains("8 bytes"), "the cap is named: {rendered}");
        assert!(rendered.contains("next:"), "one line plus next: {rendered}");
        assert!(!target.exists(), "a failed fetch never writes the target");
        let leftovers = fs::read_dir(rootfs.join("opt/data"))
            .expect("the destination directory exists")
            .count();
        assert_eq!(leftovers, 0, "the temp file is removed");

        let _ = fs::remove_dir_all(&rootfs);
    }
}
