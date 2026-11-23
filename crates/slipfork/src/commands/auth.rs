//! `slipfork auth`: pair this machine with one OSPP v1 endpoint through the real client.
//!
//! Dormant past 0.1.0: not wired into the CLI; OSPP pairing returns in a later release.

use std::process::{Command, ExitCode};
use std::sync::Arc;

use serde_json::json;
use slipfork_ospp::{
    FileDeviceKeyStore, LivePairingUrl, OsppClient, OsppEndpoint, OsppError, OsppTimeouts,
    PairPhase, PairingStore, SoftwareP256Signer, WsOsppClient,
};

use crate::commands::{Session, emit};
use crate::ui::Palette;

/// Parsed `slipfork auth` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthArgs {
    /// `--ospp-endpoint`: the highest-precedence endpoint.
    pub endpoint: Option<String>,
    /// The positional `slipfork auth <url>` endpoint.
    pub positional: Option<String>,
    /// Emit `{endpoint, account, machine}` with zero ANSI.
    pub json: bool,
}

impl Session {
    /// Pairs this machine with an OSPP v1 endpoint, or reuses the settled record.
    pub fn auth(&self, args: &AuthArgs) -> ExitCode {
        let raw = match args.endpoint.clone().or_else(|| args.positional.clone()) {
            Some(raw) => raw,
            None => match PairingStore::new(&self.host.state_dir).load() {
                Ok(Some(state)) => state.endpoint,
                Ok(None) => {
                    return self.report(
                        "no-endpoint",
                        "this daemon has no OSPP endpoint",
                        "slipfork auth <url>",
                        args.json,
                    );
                }
                Err(error) => {
                    return self.report(
                        "pairing-store",
                        &format!("cannot read the pairing record: {error}"),
                        "delete `<state_dir>/ospp.json` and run `slipfork auth <url>`",
                        args.json,
                    );
                }
            },
        };
        let endpoint = match OsppEndpoint::parse(&raw) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                return self.report(
                    "bad-endpoint",
                    &format!("`{raw}` is not a valid OSPP endpoint: {error}"),
                    "pass a `wss://` URL (or `ws://` on loopback)",
                    args.json,
                );
            }
        };
        let store = FileDeviceKeyStore::new(self.host.state_dir.join("device-key.json"));
        let signer = match SoftwareP256Signer::from_store(&store) {
            Ok(signer) => signer,
            Err(error) => return self.ospp_failure(&error, args.json),
        };
        let client = WsOsppClient::new(
            endpoint.clone(),
            Arc::new(signer),
            Some(self.host.state_dir.join("ospp.json")),
            OsppTimeouts::default(),
        );
        let color = self.color();
        let json_mode = args.json;
        let mut phase = |phase: PairPhase| {
            if json_mode {
                return;
            }
            let label = match phase {
                PairPhase::SolvingAltcha => "solving ALTCHA",
                PairPhase::AwaitingApproval => "awaiting approval",
                PairPhase::Approved => "approved",
            };
            eprintln!("{} {label}", Palette::new(color).kind("→"));
        };
        let host = self.host.clone();
        let mut on_handle = |live: &LivePairingUrl| -> Result<(), OsppError> {
            let url = live.as_str().to_owned();
            // Under `--json` stdout stays one object: the human pairing URL goes to stderr.
            if json_mode {
                eprintln!("{url}");
                return Ok(());
            }
            if host.ssh || !host.display {
                println!("{url}");
                return Ok(());
            }
            match &host.opener {
                Some(opener)
                    if Command::new(opener)
                        .arg(&url)
                        .status()
                        .is_ok_and(|s| s.success()) =>
                {
                    Ok(())
                }
                _ => {
                    println!("{url}");
                    Ok(())
                }
            }
        };
        match client.pair(&self.host.hostname, &mut phase, &mut on_handle) {
            Ok(approval) => {
                if args.json {
                    return emit(&format!(
                        "{}\n",
                        json!({
                            "endpoint": endpoint.as_str(),
                            "account": approval.account,
                            "machine": approval.machine,
                        })
                    ));
                }
                emit(&format!(
                    "{} Paired {}@{} -> {}\nnext: slipfork up\n",
                    Palette::new(color).icon_ok(),
                    approval.account,
                    approval.machine,
                    Palette::new(color).url(endpoint.as_str()),
                ))
            }
            Err(error) => self.ospp_failure(&error, args.json),
        }
    }

    fn ospp_failure(&self, error: &OsppError, json: bool) -> ExitCode {
        let text = error.to_string();
        let (message, next) = match text.split_once("\nnext: ") {
            Some((message, next)) => (message.to_owned(), next.to_owned()),
            None => (
                text,
                "run `slipfork auth <url>` again, or check the endpoint".to_owned(),
            ),
        };
        let code = error.code().unwrap_or("ospp");
        self.report(code, &message, &next, json)
    }
}
