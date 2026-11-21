//! The world's native S3 endpoint: one `TcpListener` bound inside the world netns.
//!
//! Only this composition root constructs a runtime. Library code hands back an async `serve`
//! future; the init spawns it on the injected multi-thread runtime and reaps it on teardown.

use std::net::{SocketAddr, TcpListener};

use slipfork_world::S3Server;
use slipfork_world::control::S3WorldConfig;
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::{Fault, REMEDY_RUNTIME};

/// One running native S3 server, owned by the world init.
pub(crate) struct S3Host {
    listen: SocketAddr,
    buckets: usize,
    handle: Handle,
    shutdown: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl S3Host {
    /// Binds `config.listen_addr`, creates the bucket directories, and starts serving.
    ///
    /// An ephemeral `:0` address is resolved before the server is built, so callers always learn
    /// the real port. A second `ConfigureS3` replaces the previous server.
    pub(crate) fn bind(handle: &Handle, mut config: S3WorldConfig) -> Result<Self, Fault> {
        let listener = TcpListener::bind(config.listen_addr).map_err(|source| {
            Fault::cause(
                format!("native S3 bind at `{}`", config.listen_addr),
                REMEDY_RUNTIME,
                source,
            )
        })?;
        let listen = listener
            .local_addr()
            .map_err(|source| Fault::cause("native S3 local address", REMEDY_RUNTIME, source))?;
        config.listen_addr = listen;
        let buckets = config.buckets.len();
        let server = S3Server::new(config).map_err(|error| {
            Fault::new(format!("native S3 server setup: {error}"), REMEDY_RUNTIME)
        })?;
        let (shutdown, stopped) = oneshot::channel();
        let task_handle = handle.clone();
        let join = handle.spawn(async move {
            if let Err(error) = server
                .serve(&task_handle, listener, async move {
                    let _ = stopped.await;
                })
                .await
            {
                eprintln!("slipfork-world-init: native S3 accept loop: {error}");
            }
        });
        Ok(Self {
            listen,
            buckets,
            handle: handle.clone(),
            shutdown: Some(shutdown),
            join: Some(join),
        })
    }

    /// The address the init bound; `Dial { port }` reaches it inside the world netns.
    pub(crate) const fn listen_addr(&self) -> SocketAddr {
        self.listen
    }

    /// How many buckets this endpoint serves.
    pub(crate) const fn buckets(&self) -> usize {
        self.buckets
    }

    /// Signals the accept loop and waits for it, so no task outlives teardown.
    pub(crate) fn shutdown(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = self.handle.block_on(join);
        }
    }
}
