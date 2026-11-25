//! Pure failure-text classifier for image builds: plain words, one actionable remedy.
//!
//! The daemon never colours; this module only recognises the four host-independent classes the
//! owner reports named and returns a diagnosis that names the failing instruction and the run id.
//! The CLI renders the result through its one failure block.

/// One recognised failure class: what happened and the single next step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Diagnosis {
    pub(super) summary: String,
    pub(super) next: String,
}

/// Classifies captured build output. `instruction` is the last started Dockerfile step, and
/// `run_id` is the capture a human can replay with `slipfork logs -r <id>`.
pub(super) fn classify(
    captured: &str,
    instruction: Option<&str>,
    run_id: Option<&str>,
) -> Option<Diagnosis> {
    let text = captured.to_ascii_lowercase();
    let instruction = instruction.unwrap_or("the failing instruction");
    let replay = run_id.map_or_else(String::new, |id| {
        format!("; replay with `slipfork logs -r {id}`")
    });
    if text.contains("release file")
        && (text.contains("expired")
            || text.contains("not valid yet")
            || text.contains("no longer valid")
            || text.contains("valid-until"))
    {
        return Some(Diagnosis {
            summary: format!(
                "{instruction} rejected expired apt metadata (`Valid-Until`); the baked apt lists look fresh because unpack stamped layer mtimes with `now`, so apt keeps a 304 body"
            ),
            next: format!(
                "add `apt-get update` to the failing step and retry, or retry once the layer-metadata fix ships{replay}"
            ),
        });
    }
    if text.contains("no space left on device") {
        return Some(Diagnosis {
            summary: format!("{instruction} ran out of disk space under the image store"),
            next: format!("free space under `~/.cache/slipfork`, then retry{replay}"),
        });
    }
    if text.contains("temporary failure resolving")
        || text.contains("could not resolve")
        || text.contains("connection refused")
        || text.contains("network is unreachable")
        || text.contains("failed to connect")
    {
        return Some(Diagnosis {
            summary: format!("{instruction} could not reach its package or registry host"),
            next: format!("check the host network and DNS, then run `slipfork up` again{replay}"),
        });
    }
    if text.contains("out of memory")
        || text.contains("oom-kill")
        || text.contains("oomkill")
        || text.contains("killed process")
    {
        return Some(Diagnosis {
            summary: format!("{instruction} was killed by the host out-of-memory handler"),
            next: format!(
                "lower this build step's memory use or add host memory, then retry{replay}"
            ),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::classify;

    #[test]
    fn expired_apt_metadata_names_the_instruction_and_replay() {
        let found = classify(
            "E: Release file for http://deb.debian.org/debian/dists/trixie/InRelease is expired (invalid since 12d)",
            Some("RUN apt-get update"),
            Some("3f9c21"),
        )
        .expect("expired metadata is recognised");
        assert!(found.summary.contains("RUN apt-get update"), "{found:?}");
        assert!(found.summary.contains("Valid-Until"), "{found:?}");
        assert!(found.summary.contains("unpack stamped"), "{found:?}");
        assert!(found.next.contains("apt-get update"), "{found:?}");
        assert!(found.next.contains("slipfork logs -r 3f9c21"), "{found:?}");
        assert!(!found.next.contains("docker pull"), "{found:?}");
        assert!(!found.next.contains('\u{1b}'), "plain text only");
    }

    #[test]
    fn dns_disk_and_oom_each_get_their_own_next() {
        let dns = classify("Could not resolve host: deb.debian.org", None, None)
            .expect("DNS is recognised");
        assert!(dns.next.contains("network and DNS"), "{dns:?}");

        let disk = classify("write failed: No space left on device", None, None)
            .expect("disk exhaustion is recognised");
        assert!(disk.next.contains("free space"), "{disk:?}");

        let oom = classify("Killed process 42 (cargo) total-vm:...", None, None)
            .expect("OOM is recognised");
        assert!(oom.next.contains("memory"), "{oom:?}");
    }

    #[test]
    fn an_unrelated_failure_returns_none() {
        assert!(classify("crun exited with code 1", None, None).is_none());
    }
}
