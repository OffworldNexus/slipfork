//! Per-`COPY`/`ADD` source Merkle over the paths an instruction really matches.
//!
//! The canonical owner of source resolution is [`crate::copy::matched_entries`], so a source key
//! always agrees with the bytes a copy writes: `.dockerignore` pruning, wildcard expansion, and
//! directory prefixes all come from one walk. A file a `COPY` never names cannot move its step key.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::context::{BuildContext, ContextEntry};
use crate::copy;
use crate::digest::hex;
use crate::error::{Error, Result};
use crate::fetch;

/// The inputs a `COPY` step hashes: one source Merkle over every matched entry.
pub(crate) fn copy_inputs(
    context: &BuildContext,
    sources: &[String],
) -> Result<Vec<(String, String)>> {
    Ok(vec![(
        "sources".to_owned(),
        source_merkle(context, sources)?,
    )])
}

/// The inputs an `ADD` step hashes: its local source Merkle plus the remote URLs it streams.
///
/// Docker's `--checksum` is not parsed by this builder, so the URL itself is the remote identity:
/// an unchanged `ADD <url>` line stays sealed even when the remote bytes change.
pub(crate) fn add_inputs(
    context: &BuildContext,
    sources: &[String],
) -> Result<Vec<(String, String)>> {
    let mut local = Vec::new();
    let mut remote = Vec::new();
    for source in sources {
        if fetch::is_http(source) {
            remote.push(source.clone());
        } else {
            local.push(source.clone());
        }
    }
    let mut inputs = Vec::new();
    if !local.is_empty() {
        inputs.push(("sources".to_owned(), source_merkle(context, &local)?));
    }
    if !remote.is_empty() {
        inputs.push(("remote".to_owned(), remote.join("\n")));
    }
    Ok(inputs)
}

/// `sha256` over `(relative path, mode, file bytes or symlink target)` for every matched entry.
pub(crate) fn source_merkle(context: &BuildContext, sources: &[String]) -> Result<String> {
    let mut selected: BTreeMap<PathBuf, &ContextEntry> = BTreeMap::new();
    for source in sources {
        for entry in copy::matched_entries(context, source)? {
            selected.insert(entry.relative.clone(), entry);
        }
    }
    let mut hasher = Sha256::new();
    for entry in selected.values() {
        let metadata = fs::symlink_metadata(&entry.absolute)
            .map_err(|source| Error::io("inspect", &entry.absolute, source))?;
        hasher.update(entry.relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update(metadata.mode().to_be_bytes());
        hasher.update([0]);
        if entry.is_dir {
            continue;
        }
        let bytes = if metadata.file_type().is_symlink() {
            fs::read_link(&entry.absolute)
                .map_err(|source| Error::io("readlink", &entry.absolute, source))?
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else {
            fs::read(&entry.absolute)
                .map_err(|source| Error::io("read", &entry.absolute, source))?
        };
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(&bytes);
    }
    Ok(hex(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{copy_inputs, source_merkle};
    use crate::context::BuildContext;

    fn lab(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let root = PathBuf::from("/var/tmp").join(format!(
            "slipfork-sources-{prefix}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("the source lab needs real disk");
        root
    }

    fn merkle(root: &std::path::Path, sources: &[&str]) -> String {
        let context = BuildContext::load(root).expect("load the real context");
        let sources: Vec<String> = sources.iter().map(|source| (*source).to_owned()).collect();
        source_merkle(&context, &sources).expect("hash the matched entries")
    }

    #[test]
    fn only_matched_files_move_the_merkle() {
        let root = lab("matched");
        std::fs::create_dir_all(root.join("app")).expect("create app");
        std::fs::write(root.join("app/copied.txt"), "one").expect("write copied");
        std::fs::write(root.join("other.txt"), "outside").expect("write other");
        let first = merkle(&root, &["app"]);
        std::fs::write(root.join("other.txt"), "edited outside the COPY").expect("edit other");
        assert_eq!(
            first,
            merkle(&root, &["app"]),
            "an edited non-COPY'd file never moves the source key"
        );
        std::fs::write(root.join("app/copied.txt"), "two").expect("edit copied");
        assert_ne!(first, merkle(&root, &["app"]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn symlink_targets_and_dockerignore_are_hashed() {
        let root = lab("symlink");
        std::fs::create_dir_all(root.join("app")).expect("create app");
        std::fs::write(root.join("app/real.txt"), "real").expect("write real");
        symlink("real.txt", root.join("app/link")).expect("plant a symlink");
        std::fs::write(root.join("app/ignored.log"), "noise").expect("write ignored");
        std::fs::write(root.join(".dockerignore"), "*.log\n").expect("write ignore");
        let first = merkle(&root, &["app"]);
        std::fs::remove_file(root.join("app/link")).expect("drop the link");
        symlink("other.txt", root.join("app/link")).expect("retarget the symlink");
        assert_ne!(first, merkle(&root, &["app"]), "a symlink target is hashed");
        let retargeted = merkle(&root, &["app"]);
        std::fs::write(root.join("app/ignored.log"), "more noise").expect("edit ignored");
        assert_eq!(
            retargeted,
            merkle(&root, &["app"]),
            "`.dockerignore` keeps the log out of the key"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn copy_inputs_carry_one_sources_pair() {
        let root = lab("inputs");
        std::fs::write(root.join("a.txt"), "a").expect("write a");
        let context = BuildContext::load(&root).expect("load the context");
        let inputs = copy_inputs(&context, &["a.txt".to_owned()]).expect("hash inputs");
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].0, "sources");
        assert_eq!(inputs[0].1.len(), 64);
        let _ = std::fs::remove_dir_all(&root);
    }
}
