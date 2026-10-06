//! Private hub mode of the shared executable. No HTTP or discovery listener.
use anyhow::{Context, Result, bail};
use regain_core::CancellationToken;
use regain_hub::{
    config::ConfigStore, credentials::CredentialStore, endpoint::Endpoint, factory::NoCredentials,
    host, ipc::Limits, native::NativeRuntime, runtime::HubRuntime, safety::MonotonicClock,
    service::HubService,
};
use std::{future::Future, path::Path, sync::Arc, time::Duration};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub protocol_version: u16,
    pub instance_id: uuid::Uuid,
    pub host_instance: uuid::Uuid,
    pub configuration_revision: uuid::Uuid,
    pub transport: &'static str,
    pub address: std::path::PathBuf,
    /// A launched candidate, not an ownership credential. Another launcher may
    /// win the lock. Frontends must never kill this PID on disconnect.
    pub started_process_id: Option<u32>,
}

/// Locate or launch the shared host. There is at most one launch attempt; a held
/// ownership lock means wait for that owner, never start a replacement on timeout.
pub async fn attach(config: &Path, workers: &Path, executable: &Path) -> Result<Attachment> {
    let endpoint = Endpoint::for_config(config).context("Resolve private hub endpoint")?;
    let store = ConfigStore::load(endpoint.config_path())
        .map_err(|_| anyhow::anyhow!("Cannot load a valid hub configuration"))?;
    let instance = store.snapshot().instance_id;
    let available = endpoint.try_lock().context("Check hub ownership")?;
    let started_process_id = if let Some(owner) = available {
        // Release before launching; the host itself arbitrates the race. A
        // simultaneous loser only probes and cannot start duplicate sources.
        drop(owner);
        Some(
            regain_hub::launch::spawn_host(executable, endpoint.config_path(), workers)
                .await
                .context("Start the shared hub host")?,
        )
    } else {
        None
    };
    let hello = host::probe(&endpoint, instance, Duration::from_secs(10))
        .await
        .context(
            "Shared hub did not pass its readiness check; no automatic restart was attempted",
        )?;
    Ok(Attachment {
        protocol_version: hello.protocol_version,
        instance_id: hello.instance_id,
        host_instance: hello.host_instance,
        configuration_revision: hello.configuration_revision,
        transport: if cfg!(windows) {
            "namedPipe"
        } else {
            "unixSocket"
        },
        address: endpoint.address(),
        started_process_id,
    })
}

pub async fn run(
    config: &Path,
    native: NativeRuntime,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    let endpoint = Endpoint::for_config(config).context("Resolve private hub endpoint")?;
    // Lock before loading/preparing any source. A losing launcher only probes
    // the existing host and exits; it never builds a second set of source actors.
    let owner = endpoint.try_lock().context("Acquire hub ownership")?;
    let store = ConfigStore::load(endpoint.config_path())
        .map_err(|_| anyhow::anyhow!("Cannot load a valid hub configuration"))?;
    let config = store.snapshot();
    let Some(owner) = owner else {
        let hello = host::probe(&endpoint, config.instance_id, Duration::from_secs(10))
            .await
            .context("Existing hub did not pass its readiness check")?;
        println!("Regain hub already running: {}", hello.host_instance);
        return Ok(());
    };
    let listener = owner.bind().context("Bind private hub endpoint")?;
    // A headless account without a home directory can still host sources that
    // do not require credentials. References fail closed in that case.
    let credentials = CredentialStore::for_endpoint(&endpoint).ok().map(Arc::new);
    let provider = credentials.clone();
    let builder: Arc<regain_hub::service::RuntimeBuilder> = Arc::new(move |config| {
        HubRuntime::build(
            config,
            &native,
            provider
                .as_deref()
                .map(|store| store as &dyn regain_hub::factory::CredentialProvider)
                .unwrap_or(&NoCredentials),
            Arc::new(MonotonicClock::default()),
        )
    });
    let state = match credentials {
        Some(credentials) => HubService::persistent_with_credentials(store, builder, credentials),
        None => HubService::persistent(store, builder),
    }
    .map_err(|errors| {
        anyhow::anyhow!(
            "Hub configuration cannot be hosted: {}",
            errors
                .iter()
                .map(|error| format!("{}: {}", error.path, error.message))
                .collect::<Vec<_>>()
                .join("; ")
        )
    })?;
    let stop = CancellationToken::new();
    let service = host::serve_service(listener, state.clone(), Limits::default(), stop.clone());
    tokio::pin!(service);
    println!("Regain hub ready: {} (local IPC only)", state.host_id());
    let result = tokio::select! {
        result = &mut service => result,
        _ = shutdown => { stop.cancel(); service.await }
    };
    if let Err(error) = result {
        bail!("{error}; {} source cleanup errors", error.cleanup.len());
    }
    Ok(())
}
