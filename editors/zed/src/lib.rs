//! Zed extension for `Forkfile`: the grammar's queries, plus the standalone
//! `slipfork-lsp` language server.
//!
//! Resolution order is exactly three steps and nothing else: the
//! `lsp.slipfork-lsp.binary.path` setting, then `slipfork-lsp` on the shell
//! `PATH`, then one GitHub Release download for this platform — unpacked once
//! into a version-less work directory and reused from disk on every later
//! start, so an offline editor still gets a server.
//!
//! Zed spawns the returned command; the wasm never executes anything. The child
//! gets an allowlist of two names (`PATH`, and `SYSTEMROOT` on Windows) rather
//! than the whole captured shell environment: the server reads no environment,
//! so a token the developer exported must not reach it.

use zed_extension_api as zed;

/// The language server's id: the `extension.toml` table key, and the name the
/// `lsp` settings are looked up under.
const LANGUAGE_SERVER_ID: &str = "slipfork-lsp";

/// The repository the release assets come from.
const REPOSITORY: &str = "OffworldNexus/slipfork";

/// Real releases only, and they must carry assets.
const RELEASE_OPTIONS: zed::GithubReleaseOptions = zed::GithubReleaseOptions {
    require_assets: true,
    pre_release: false,
};

/// The one directory the download extracts into, relative to the extension's
/// work directory. It carries **no** version: the cache has to answer before any
/// network call, and `<version>` exists only in GitHub's reply — a versioned name
/// would put the network in front of a binary that is already on disk.
const EXTRACT_DIR: &str = "slipfork-lsp";

/// The only environment names any step hands the server. `SYSTEMROOT` is what
/// Windows resolves its own DLLs with and is absent on Unix, so this is one list
/// and not a `cfg` fork.
const ENV_ALLOWLIST: [&str; 2] = ["PATH", "SYSTEMROOT"];

/// The sentence every failure ends with: what is built, and the way past it.
const FAILURE_HINT: &str = "releases cover Linux x86_64/aarch64, macOS \
     x86_64/aarch64 and Windows x86_64/aarch64; set \
     `lsp.slipfork-lsp.binary.path` to a `slipfork-lsp` you built";

/// One release asset, and how it unpacks.
struct Asset {
    /// The asset's file name on the release page.
    name: &'static str,
    /// How `download_file` extracts it: `Zip` only on Windows.
    file_type: zed::DownloadedFileType,
    /// The executable inside the archive's one top-level directory.
    executable: &'static str,
}

/// The asset for one `current_platform()` pair, or `None` when no leg is built.
///
/// Six arms and no default: a pair this table does not name has no release
/// artifact, and a silent fallback would download another machine's binary.
fn asset_for(os: zed::Os, architecture: zed::Architecture) -> Option<Asset> {
    let (name, file_type, executable) = match (os, architecture) {
        (zed::Os::Linux, zed::Architecture::X8664) => (
            "slipfork-lsp-linux-x86_64.tar.gz",
            zed::DownloadedFileType::GzipTar,
            "slipfork-lsp",
        ),
        (zed::Os::Linux, zed::Architecture::Aarch64) => (
            "slipfork-lsp-linux-aarch64.tar.gz",
            zed::DownloadedFileType::GzipTar,
            "slipfork-lsp",
        ),
        (zed::Os::Mac, zed::Architecture::X8664) => (
            "slipfork-lsp-darwin-x86_64.tar.gz",
            zed::DownloadedFileType::GzipTar,
            "slipfork-lsp",
        ),
        (zed::Os::Mac, zed::Architecture::Aarch64) => (
            "slipfork-lsp-darwin-aarch64.tar.gz",
            zed::DownloadedFileType::GzipTar,
            "slipfork-lsp",
        ),
        (zed::Os::Windows, zed::Architecture::X8664) => (
            "slipfork-lsp-windows-x86_64.zip",
            zed::DownloadedFileType::Zip,
            "slipfork-lsp.exe",
        ),
        (zed::Os::Windows, zed::Architecture::Aarch64) => (
            "slipfork-lsp-windows-aarch64.zip",
            zed::DownloadedFileType::Zip,
            "slipfork-lsp.exe",
        ),
        _ => return None,
    };
    Some(Asset {
        name,
        file_type,
        executable,
    })
}

/// The executable's path, relative to the extension's work directory:
/// `slipfork-lsp/<asset stem>/<executable>`.
///
/// The archive keeps its own top-level directory (`download_file` extracts
/// verbatim), so the path is the extract directory, that directory, and the
/// executable.
fn executable_path(asset: &Asset) -> String {
    let directory = asset
        .name
        .strip_suffix(".tar.gz")
        .or_else(|| asset.name.strip_suffix(".zip"))
        .unwrap_or(asset.name);
    format!("{EXTRACT_DIR}/{directory}/{}", asset.executable)
}

/// The child's environment: the allowlist, read out of the shell environment Zed
/// captured. The filter is the point — everything else the developer exported is
/// dropped.
fn env_allowlist(worktree: &zed::Worktree) -> zed::EnvVars {
    worktree
        .shell_env()
        .into_iter()
        .filter(|(name, _)| ENV_ALLOWLIST.contains(&name.as_str()))
        .collect()
}

/// One server invocation: the path, its arguments, and nothing but the
/// environment allowlist. Every resolver returns through here, so the child's
/// environment cannot drift between them.
fn command(path: String, args: Vec<String>, worktree: &zed::Worktree) -> zed::Command {
    zed::Command {
        command: path,
        args,
        env: env_allowlist(worktree),
    }
}

/// Step 1: the `lsp.slipfork-lsp.binary.path` setting, if the developer set one.
///
/// `arguments` stay whatever the settings carry, because a `path` may point at a
/// wrapper script. The settings' `env` map is deliberately ignored: the child
/// gets the allowlist.
fn from_settings(worktree: &zed::Worktree) -> Option<zed::Command> {
    let settings = zed::settings::LspSettings::for_worktree(LANGUAGE_SERVER_ID, worktree).ok()?;
    let binary = settings.binary?;
    let path = binary.path?;
    Some(command(
        path,
        binary.arguments.unwrap_or_default(),
        worktree,
    ))
}

/// Step 2: a server already on the shell `PATH`.
fn from_path(worktree: &zed::Worktree) -> Option<zed::Command> {
    let path = worktree.which(LANGUAGE_SERVER_ID)?;
    Some(command(path, Vec::new(), worktree))
}

/// Step 3: the server this extension unpacked earlier, then one download.
fn from_release(id: &zed::LanguageServerId, worktree: &zed::Worktree) -> zed::Result<zed::Command> {
    let (os, architecture) = zed::current_platform();
    let Some(asset) = asset_for(os, architecture) else {
        return Err(failed(
            id,
            format!("no `slipfork-lsp` build for {os:?} {architecture:?}; {FAILURE_HINT}"),
        ));
    };
    let binary = executable_path(&asset);
    if std::fs::metadata(&binary).is_ok_and(|entry| entry.is_file()) {
        return Ok(command(binary, Vec::new(), worktree));
    }
    zed::set_language_server_installation_status(
        id,
        &zed::LanguageServerInstallationStatus::Downloading,
    );
    let release = match zed::latest_github_release(REPOSITORY, RELEASE_OPTIONS) {
        Ok(release) => release,
        Err(error) => {
            return Err(failed(
                id,
                format!("no release from {REPOSITORY}: {error}; {FAILURE_HINT}"),
            ));
        }
    };
    let Some(asset_url) = release
        .assets
        .iter()
        .find(|candidate| candidate.name == asset.name)
    else {
        return Err(failed(
            id,
            format!(
                "release {} carries no {}; {FAILURE_HINT}",
                release.version, asset.name
            ),
        ));
    };
    if let Err(error) = zed::download_file(&asset_url.download_url, EXTRACT_DIR, asset.file_type) {
        return Err(failed(
            id,
            format!("downloading {} failed: {error}; {FAILURE_HINT}", asset.name),
        ));
    }
    // Unconditional: a no-op off Unix, so there is no `cfg` and no second path.
    if let Err(error) = zed::make_file_executable(&binary) {
        return Err(failed(
            id,
            format!("{binary} was unpacked but is not executable: {error}; {FAILURE_HINT}"),
        ));
    }
    Ok(command(binary, Vec::new(), worktree))
}

/// Reports `message` to Zed as the installation failure and returns it, so every
/// `Err` in this extension is one sentence the developer reads in one line.
fn failed(id: &zed::LanguageServerId, message: String) -> String {
    zed::set_language_server_installation_status(
        id,
        &zed::LanguageServerInstallationStatus::Failed(message.clone()),
    );
    message
}

struct ForkfileExtension;

impl zed::Extension for ForkfileExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        // The whole resolution order, once. Each step returns early, so the
        // network is only touched when nothing on this machine can answer.
        if let Some(command) = from_settings(worktree) {
            return Ok(command);
        }
        if let Some(command) = from_path(worktree) {
            return Ok(command);
        }
        from_release(language_server_id, worktree)
    }
}

zed::register_extension!(ForkfileExtension);

#[cfg(test)]
mod tests {
    use super::*;

    /// Every pair, with the `Makefile`'s `lsp-assets` suffix and whether it is a
    /// Windows build.
    const SUPPORTED: [(zed::Os, zed::Architecture, &str, bool); 6] = [
        (
            zed::Os::Linux,
            zed::Architecture::X8664,
            "linux-x86_64",
            false,
        ),
        (
            zed::Os::Linux,
            zed::Architecture::Aarch64,
            "linux-aarch64",
            false,
        ),
        (
            zed::Os::Mac,
            zed::Architecture::X8664,
            "darwin-x86_64",
            false,
        ),
        (
            zed::Os::Mac,
            zed::Architecture::Aarch64,
            "darwin-aarch64",
            false,
        ),
        (
            zed::Os::Windows,
            zed::Architecture::X8664,
            "windows-x86_64",
            true,
        ),
        (
            zed::Os::Windows,
            zed::Architecture::Aarch64,
            "windows-aarch64",
            true,
        ),
    ];

    #[test]
    fn every_supported_pair_has_its_own_archive_and_executable_path() {
        for (os, architecture, suffix, windows) in SUPPORTED {
            let asset = asset_for(os, architecture).expect("supported pair");
            let archive = if windows { "zip" } else { "tar.gz" };
            assert_eq!(asset.name, format!("slipfork-lsp-{suffix}.{archive}"));
            assert_eq!(asset.file_type == zed::DownloadedFileType::Zip, windows);
            assert_eq!(
                asset.executable,
                if windows {
                    "slipfork-lsp.exe"
                } else {
                    "slipfork-lsp"
                }
            );
            assert_eq!(
                executable_path(&asset),
                format!("slipfork-lsp/slipfork-lsp-{suffix}/{}", asset.executable)
            );
        }
    }
}
