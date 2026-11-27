//! Real-disk `COPY`/`ADD` lab fixtures shared by the focused test modules.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::context::BuildContext;
use crate::dockerfile::Owner;

mod paths;
mod urls;

fn lab(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let root = PathBuf::from("/var/tmp").join(format!(
        "slipfork-image-{prefix}-{}-{nanos}",
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

fn context(files: &[(&str, &str)]) -> (PathBuf, BuildContext) {
    let root = lab("context");
    for (relative, body) in files {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create a context directory");
        }
        fs::write(&path, body).expect("write a context file");
    }
    let context = BuildContext::load(&root).expect("load the real build context");
    (root, context)
}
