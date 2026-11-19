//! Protocol sessions: authentication, pairing, subscription, and dump requests.

use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Instant;

use slipfork_forkfile::{EntropySource, EvaluatedFork};
use slipfork_ospp::wire::{
    AuthResponse, Challenge, ClientFrame, Encryption, ServerFrame, auth_transcript,
    pairing_transcript,
};
use slipfork_ospp::{
    AUTH_METHOD_P256, ENCRYPTION_METHOD_HPKE_P256, OsppClaims, OsppError, PROTOCOL_VERSION,
    altcha_challenge, altcha_challenge_hash, codes, verify_altcha, verify_device_signature,
};
use std::net::TcpStream;
use tungstenite::WebSocket;

use super::claims::resolve_components;
use super::env::seal_components;
use super::frames::{FilledRng, frame_type, read_client, send_error, send_frame};
use super::{
    ActiveSubscription, DumpEntry, IO_TIMEOUT, PairOutcome, PairingRecord, Shared,
    SubscriptionRecord, lock,
};

pub(super) fn serve(
    socket: &mut WebSocket<TcpStream>,
    shared: &Arc<Shared>,
) -> Result<(), OsppError> {
    let first = loop {
        if shared.stop.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(());
        }
        match read_client(socket)? {
            Some(frame) => break frame,
            None => continue,
        }
    };
    lock(&shared.state)
        .frames
        .push(frame_type(&first).to_owned());
    match first {
        ClientFrame::Hello {
            version,
            auth_methods,
        } => authenticated_session(socket, shared, version, auth_methods),
        ClientFrame::PairHello {
            version,
            key_methods,
        } => pairing_session(socket, shared, version, key_methods),
        other => {
            let _ = send_error(
                socket,
                None,
                codes::BAD_FRAME,
                "expected hello or pair_hello",
            );
            Err(OsppError::Protocol(format!(
                "unexpected first frame {}",
                frame_type(&other)
            )))
        }
    }
}

fn authenticated_session(
    socket: &mut WebSocket<TcpStream>,
    shared: &Arc<Shared>,
    version: u32,
    auth_methods: Vec<String>,
) -> Result<(), OsppError> {
    if version != PROTOCOL_VERSION {
        let _ = send_error(
            socket,
            None,
            codes::UNSUPPORTED_VERSION,
            "only protocol version 2 is supported",
        );
        return Err(OsppError::Protocol("unsupported version".to_owned()));
    }
    if !auth_methods.iter().any(|method| method == AUTH_METHOD_P256) {
        let _ = send_error(
            socket,
            None,
            codes::UNSUPPORTED_AUTH,
            "only p256 authentication is supported",
        );
        return Err(OsppError::Protocol("unsupported auth method".to_owned()));
    }

    let mut server_nonce = [0u8; 32];
    shared.entropy.fill_bytes(&mut server_nonce)?;
    send_frame(
        socket,
        &ServerFrame::AuthChallenge {
            method: AUTH_METHOD_P256.to_owned(),
            challenge: Challenge {
                nonce: hex::encode(server_nonce),
            },
        },
    )?;
    let response = match read_client(socket)? {
        Some(ClientFrame::AuthResponse { response }) => response,
        _ => {
            let _ = send_error(socket, None, codes::BAD_FRAME, "expected auth_response");
            return Err(OsppError::Protocol("expected auth_response".to_owned()));
        }
    };
    lock(&shared.state).frames.push("auth_response".to_owned());
    authenticate_response(socket, shared, &server_nonce, &response, false)?;
    send_frame(
        socket,
        &ServerFrame::AuthOk {
            encryption_methods: vec![ENCRYPTION_METHOD_HPKE_P256.to_owned()],
        },
    )?;
    session_loop(socket, shared)
}

fn authenticate_response(
    socket: &mut WebSocket<TcpStream>,
    shared: &Arc<Shared>,
    server_nonce: &[u8; 32],
    response: &AuthResponse,
    pairing: bool,
) -> Result<(), OsppError> {
    let client_nonce = hex::decode(&response.client_nonce)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .ok_or_else(|| OsppError::Protocol("client_nonce must be 32 bytes".to_owned()))?;
    if !pairing {
        let accepted = lock(&shared.config).accepted_keys.clone();
        if !accepted.iter().any(|key| key == &response.public_key) {
            let _ = send_error(
                socket,
                None,
                codes::UNAUTHORIZED_KEY,
                "machine key is not enrolled",
            );
            return Err(OsppError::Protocol("unauthorized key".to_owned()));
        }
    }
    let transcript = if pairing {
        pairing_transcript(server_nonce, &client_nonce)
    } else {
        auth_transcript(server_nonce, &client_nonce)
    };
    let signature = slipfork_ospp::DeviceSignature::from_hex(&response.signature)
        .map_err(|_| OsppError::InvalidSignature)?;
    if verify_device_signature(&response.public_key, &transcript, &signature).is_err() {
        let _ = send_error(socket, None, codes::BAD_PROOF, "signature did not verify");
        return Err(OsppError::InvalidSignature);
    }
    Ok(())
}

fn session_loop(socket: &mut WebSocket<TcpStream>, shared: &Arc<Shared>) -> Result<(), OsppError> {
    let (outbound, outbound_rx) = std::sync::mpsc::channel();
    loop {
        if shared.stop.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(());
        }
        while let Ok(frame) = outbound_rx.try_recv() {
            send_frame(socket, &frame)?;
        }
        let frame = match read_client(socket)? {
            Some(frame) => frame,
            None => continue,
        };
        lock(&shared.state)
            .frames
            .push(frame_type(&frame).to_owned());
        match frame {
            ClientFrame::Subscribe {
                id,
                remote,
                encryption,
                forkfile,
                claims,
            } => subscribe(
                socket, shared, &outbound, id, remote, encryption, *forkfile, claims,
            )?,
            ClientFrame::Unsubscribe { id } => {
                lock(&shared.subscriptions).remove(&id);
                lock(&shared.state).subscriptions.remove(&id);
            }
            ClientFrame::DumpRequest {
                id,
                subscription_id,
                resource,
            } => dump(socket, shared, id, subscription_id, resource)?,
            other => {
                let _ = send_error(
                    socket,
                    None,
                    codes::BAD_FRAME,
                    "frame is not valid in an authenticated session",
                );
                return Err(OsppError::Protocol(format!(
                    "unexpected frame {}",
                    frame_type(&other)
                )));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn subscribe(
    socket: &mut WebSocket<TcpStream>,
    shared: &Arc<Shared>,
    outbound: &Sender<ServerFrame>,
    id: String,
    remote: String,
    encryption: Encryption,
    forkfile: EvaluatedFork,
    claims: OsppClaims,
) -> Result<(), OsppError> {
    if encryption.method != ENCRYPTION_METHOD_HPKE_P256 {
        send_error(
            socket,
            Some(&id),
            codes::UNSUPPORTED_ENCRYPTION,
            "only hpke-p256 is supported",
        )?;
        return Ok(());
    }
    let config = lock(&shared.config).clone();
    let resolved = match resolve_components(&config, &claims, &forkfile) {
        Ok(resolved) => resolved,
        Err((code, message)) => {
            send_error(socket, Some(&id), &code, &message)?;
            return Ok(());
        }
    };
    let components = seal_components(
        &resolved,
        &encryption.public_key,
        &mut FilledRng::new(&shared.entropy, 8192)?,
    )?;
    send_frame(
        socket,
        &ServerFrame::EnvResolved {
            id: id.clone(),
            components,
        },
    )?;
    let record = SubscriptionRecord {
        id: id.clone(),
        remote,
        forkfile,
        claims,
        public_key: encryption.public_key,
    };
    lock(&shared.subscriptions).insert(
        id.clone(),
        ActiveSubscription {
            record: record.clone(),
            outbound: outbound.clone(),
        },
    );
    lock(&shared.state).subscriptions.insert(id, record);
    Ok(())
}

fn dump(
    socket: &mut WebSocket<TcpStream>,
    shared: &Arc<Shared>,
    id: String,
    subscription_id: String,
    resource: String,
) -> Result<(), OsppError> {
    let public_key = lock(&shared.subscriptions)
        .get(&subscription_id)
        .map(|subscription| subscription.record.public_key.clone());
    let bytes = lock(&shared.config).dumps.get(&resource).cloned();
    let (Some(public_key), Some(bytes)) = (public_key, bytes) else {
        send_error(
            socket,
            Some(&id),
            codes::DUMP_UNAVAILABLE,
            "no dump is available for that resource",
        )?;
        return Ok(());
    };
    let mut token_bytes = [0u8; 16];
    shared.entropy.fill_bytes(&mut token_bytes)?;
    let token = hex::encode(token_bytes);
    lock(&shared.dumps).insert(
        token.clone(),
        DumpEntry {
            public_key,
            subscription_id,
            resource,
            bytes,
        },
    );
    lock(&shared.state).dump_urls += 1;
    send_frame(
        socket,
        &ServerFrame::DumpReady {
            id,
            url: format!("http://{}/dumps/{}", shared.http_addr, token),
        },
    )
}

fn pairing_session(
    socket: &mut WebSocket<TcpStream>,
    shared: &Arc<Shared>,
    version: u32,
    key_methods: Vec<String>,
) -> Result<(), OsppError> {
    if version != PROTOCOL_VERSION {
        send_error(
            socket,
            None,
            codes::UNSUPPORTED_VERSION,
            "only protocol version 2 is supported",
        )?;
        return Err(OsppError::Protocol("unsupported version".to_owned()));
    }
    if !key_methods.iter().any(|method| method == AUTH_METHOD_P256) {
        send_error(
            socket,
            None,
            codes::UNSUPPORTED_AUTH,
            "only p256 key proof is supported",
        )?;
        return Err(OsppError::Protocol("unsupported key method".to_owned()));
    }

    let mut server_nonce = [0u8; 32];
    shared.entropy.fill_bytes(&mut server_nonce)?;
    let mut salt_bytes = [0u8; 16];
    shared.entropy.fill_bytes(&mut salt_bytes)?;
    let salt = hex::encode(salt_bytes);
    let mut number_bytes = [0u8; 1];
    shared.entropy.fill_bytes(&mut number_bytes)?;
    let number = u64::from(number_bytes[0] % 64);
    let challenge = altcha_challenge_hash(&salt, number);
    send_frame(
        socket,
        &ServerFrame::PairChallenge {
            method: AUTH_METHOD_P256.to_owned(),
            altcha: altcha_challenge(&salt, number, 64),
            challenge: Challenge {
                nonce: hex::encode(server_nonce),
            },
        },
    )?;

    let (altcha, hostname, response) = match read_client(socket)? {
        Some(ClientFrame::PairBegin {
            altcha,
            hostname,
            response,
        }) => (altcha, hostname, response),
        _ => {
            send_error(socket, None, codes::BAD_FRAME, "expected pair_begin")?;
            return Err(OsppError::Protocol("expected pair_begin".to_owned()));
        }
    };
    lock(&shared.state).frames.push("pair_begin".to_owned());
    if !verify_altcha(&altcha, &salt, &challenge) {
        send_error(socket, None, codes::BAD_PROOF, "altcha proof was not valid")?;
        return Err(OsppError::Protocol("bad altcha proof".to_owned()));
    }
    authenticate_response(socket, shared, &server_nonce, &response, true)?;

    let mut handle_bytes = [0u8; 32];
    shared.entropy.fill_bytes(&mut handle_bytes)?;
    let handle = hex::encode(handle_bytes);
    send_frame(
        socket,
        &ServerFrame::PairHandle {
            url: format!("http://{}/pair", shared.http_addr),
            handle: handle.clone(),
        },
    )?;
    lock(&shared.state).pairings.push(PairingRecord {
        handle: handle.clone(),
        hostname,
        public_key: response.public_key,
        approved: false,
    });

    let config = lock(&shared.config).clone();
    if config.auto_approve {
        lock(&shared.decisions).insert(handle.clone(), config.pair_outcome.clone());
    }
    let deadline = Instant::now() + IO_TIMEOUT;
    loop {
        if shared.stop.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(());
        }
        if let Some(outcome) = lock(&shared.decisions).remove(&handle) {
            match outcome {
                PairOutcome::Approve { account, machine } => {
                    send_frame(socket, &ServerFrame::PairApproved { account, machine })?;
                    if let Some(record) = lock(&shared.state)
                        .pairings
                        .iter_mut()
                        .find(|record| record.handle == handle)
                    {
                        record.approved = true;
                    }
                    return Ok(());
                }
                PairOutcome::Reject { code } => {
                    send_error(socket, None, &code, "pairing was rejected")?;
                    return Err(OsppError::Protocol("pairing rejected".to_owned()));
                }
            }
        }
        if Instant::now() >= deadline {
            send_error(socket, None, codes::THROTTLED, "pairing verdict timed out")?;
            return Err(OsppError::Timeout);
        }
        let _ = read_client(socket)?;
    }
}
