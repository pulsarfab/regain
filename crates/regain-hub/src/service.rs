//! Revisioned runtime publication. Filesystem work and device drain never hold
//! the service state mutex; accepted apply operations outlive their RPC waiter.
use crate::{
    config::{ApplyError, ConfigStore, HubConfig, SourceBackend},
    credentials::{
        CredentialError, CredentialStatus, CredentialStore, DeleteOutcome, SecretAuthorization,
    },
    parameters::FieldError,
    runtime::{ClientSession, HubRuntime},
    source::{ErrorKind, SourceError},
};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub type RuntimeBuilder =
    dyn Fn(HubConfig) -> Result<Arc<HubRuntime>, Vec<FieldError>> + Send + Sync;

#[derive(Debug)]
pub enum UpdateError {
    Invalid(Vec<FieldError>),
    Conflict,
    Connected,
    Busy,
    Io,
    Unsupported,
    Stopped,
    Task,
}
impl From<ApplyError> for UpdateError {
    fn from(error: ApplyError) -> Self {
        match error {
            ApplyError::Invalid(errors) => Self::Invalid(errors),
            ApplyError::Conflict => Self::Conflict,
            ApplyError::Connected => Self::Connected,
            ApplyError::Io(_) => Self::Io,
            ApplyError::Committed { .. } => Self::Task,
        }
    }
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum Phase {
    Ready,
    Applying,
    Blocked,
    Stopped,
}
struct State {
    runtime: Arc<HubRuntime>,
    phase: Phase,
    cleanup: Vec<(Uuid, SourceError)>,
    persistence_warning: Option<&'static str>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyOutcome {
    pub configuration_revision: Uuid,
    pub applied: bool,
    pub ready: bool,
    pub cleanup_errors: Vec<(Uuid, SourceError)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistence_warning: Option<&'static str>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    phase: Phase,
    configuration_revision: Uuid,
    cleanup_errors: Vec<(Uuid, SourceError)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    persistence_warning: Option<&'static str>,
}
pub struct HubService {
    instance: Uuid,
    host: Uuid,
    state: Mutex<State>,
    store: Option<Arc<ConfigStore>>,
    builder: Option<Arc<RuntimeBuilder>>,
    credentials: Option<Arc<CredentialStore>>,
    update: Arc<tokio::sync::Mutex<()>>,
}
impl HubService {
    pub fn read_only(runtime: Arc<HubRuntime>) -> Arc<Self> {
        Self::new(runtime, None, None, None)
    }
    pub fn persistent(
        store: ConfigStore,
        builder: Arc<RuntimeBuilder>,
    ) -> Result<Arc<Self>, Vec<FieldError>> {
        let runtime = build_checked(&builder, &store.snapshot())?;
        Ok(Self::new(
            runtime,
            Some(Arc::new(store)),
            Some(builder),
            None,
        ))
    }
    pub fn persistent_with_credentials(
        store: ConfigStore,
        builder: Arc<RuntimeBuilder>,
        credentials: Arc<CredentialStore>,
    ) -> Result<Arc<Self>, Vec<FieldError>> {
        let runtime = build_checked(&builder, &store.snapshot())?;
        Ok(Self::new(
            runtime,
            Some(Arc::new(store)),
            Some(builder),
            Some(credentials),
        ))
    }
    fn new(
        runtime: Arc<HubRuntime>,
        store: Option<Arc<ConfigStore>>,
        builder: Option<Arc<RuntimeBuilder>>,
        credentials: Option<Arc<CredentialStore>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            instance: runtime.instance_id(),
            host: runtime.runtime_id(),
            state: Mutex::new(State {
                runtime,
                phase: Phase::Ready,
                cleanup: Vec::new(),
                persistence_warning: None,
            }),
            store,
            builder,
            credentials,
            update: Arc::new(tokio::sync::Mutex::new(())),
        })
    }
    pub fn instance_id(&self) -> Uuid {
        self.instance
    }
    pub fn host_id(&self) -> Uuid {
        self.host
    }
    pub fn can_apply(&self) -> bool {
        self.store.is_some()
    }
    pub fn configuration_capabilities(&self) -> Vec<&'static str> {
        // Setup metadata remains readable while apply/cleanup blocks operations.
        self.state
            .lock()
            .unwrap()
            .runtime
            .configuration_capabilities()
    }
    pub fn credential_description(&self) -> Option<serde_json::Value> {
        self.credentials.as_ref().map(|store| store.description())
    }
    pub async fn credential_status(
        &self,
        reference: String,
    ) -> Result<CredentialStatus, CredentialError> {
        let store = self
            .credentials
            .clone()
            .ok_or(CredentialError::Unsupported)?;
        // Reconciliation must not report absence while an earlier abandoned
        // create/delete waiter still owns an in-flight storage transaction.
        let guard = self
            .update
            .clone()
            .try_lock_owned()
            .map_err(|_| CredentialError::Busy)?;
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            store.status(&reference)
        })
        .await
        .map_err(|_| CredentialError::Unavailable)?
    }
    pub async fn create_credential(
        &self,
        authorization: SecretAuthorization,
    ) -> Result<CredentialStatus, CredentialError> {
        self.create_credential_identified(authorization, None).await
    }
    pub async fn create_credential_identified(
        &self,
        authorization: SecretAuthorization,
        reference_id: Option<Uuid>,
    ) -> Result<CredentialStatus, CredentialError> {
        let store = self
            .credentials
            .clone()
            .ok_or(CredentialError::Unsupported)?;
        let guard = self
            .update
            .clone()
            .try_lock_owned()
            .map_err(|_| CredentialError::Busy)?;
        self.runtime().map_err(|_| CredentialError::Unavailable)?;
        // The blocking task retains the update gate if its RPC waiter goes away.
        // Shutdown and configuration activation wait until storage finishes.
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            match reference_id {
                Some(id) => store.create_identified(authorization, id),
                None => store.create(authorization),
            }
        })
        .await
        .map_err(|_| CredentialError::Unavailable)?
    }
    pub async fn delete_credential(
        &self,
        reference: String,
    ) -> Result<DeleteOutcome, CredentialError> {
        let store = self
            .credentials
            .clone()
            .ok_or(CredentialError::Unsupported)?;
        let guard = self
            .update
            .clone()
            .try_lock_owned()
            .map_err(|_| CredentialError::Busy)?;
        self.runtime().map_err(|_| CredentialError::Unavailable)?;
        if self.configuration().sources.iter().any(|source| {
            matches!(&source.backend,
            SourceBackend::Alpaca { credential_reference: Some(value), .. } if value == &reference)
        }) {
            return Err(CredentialError::InUse);
        }
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            store.delete(&reference)
        })
        .await
        .map_err(|_| CredentialError::Unavailable)?
    }
    pub fn configuration(&self) -> HubConfig {
        match &self.store {
            Some(store) => store.snapshot(),
            None => self.state.lock().unwrap().runtime.configuration().clone(),
        }
    }
    pub fn status(&self) -> ServiceStatus {
        let revision = self.configuration().revision;
        let state = self.state.lock().unwrap();
        ServiceStatus {
            phase: state.phase,
            configuration_revision: revision,
            cleanup_errors: state.cleanup.clone(),
            persistence_warning: state.persistence_warning,
        }
    }
    pub fn runtime(&self) -> Result<Arc<HubRuntime>, SourceError> {
        let state = self.state.lock().unwrap();
        match state.phase {
            Phase::Ready => Ok(state.runtime.clone()),
            Phase::Applying => Err(SourceError::new(
                ErrorKind::Busy,
                "Hub configuration is being applied",
            )),
            Phase::Blocked => Err(SourceError::new(
                ErrorKind::Unavailable,
                "Configuration activation is blocked after an uncertain update or source cleanup; inspect hostStatus before restarting",
            )),
            Phase::Stopped => Err(SourceError::new(
                ErrorKind::Disconnected,
                "Hub host stopped",
            )),
        }
    }
    pub async fn apply(
        self: &Arc<Self>,
        expected: Uuid,
        candidate: HubConfig,
    ) -> Result<ApplyOutcome, UpdateError> {
        if !self.can_apply() {
            return Err(UpdateError::Unsupported);
        }
        let guard = self
            .update
            .clone()
            .try_lock_owned()
            .map_err(|_| UpdateError::Busy)?;
        let service = self.clone();
        // Dropping/timeout of an RPC waiter cannot split commit from activation.
        let outcome = tokio::spawn(async move {
            let _guard = guard;
            let worker = service.clone();
            match tokio::spawn(async move { worker.apply_inner(expected, candidate).await }).await {
                Ok(result) => result,
                Err(_) => {
                    service.block_after_unknown_update().await;
                    Err(UpdateError::Task)
                }
            }
        })
        .await;
        outcome.map_err(|_| UpdateError::Task)?
    }
    async fn block_after_unknown_update(&self) {
        let runtime = {
            let mut state = self.state.lock().unwrap();
            if !matches!(state.phase, Phase::Stopped) {
                state.phase = Phase::Blocked;
            }
            state.runtime.clone()
        };
        let errors = runtime.shutdown().await.err().unwrap_or_default();
        self.state.lock().unwrap().cleanup = errors;
    }
    async fn apply_inner(
        &self,
        expected: Uuid,
        candidate: HubConfig,
    ) -> Result<ApplyOutcome, UpdateError> {
        let previous = self.runtime().map_err(|_| UpdateError::Stopped)?;
        let _quiescent = previous.quiesce().map_err(|_| UpdateError::Connected)?;
        self.state.lock().unwrap().phase = Phase::Applying;
        let prepared = self.prepare_and_commit(expected, candidate).await;
        let (next, persistence_warning) = match prepared {
            Ok(next) => next,
            Err(error) => {
                if matches!(error, UpdateError::Task) {
                    self.block_after_unknown_update().await;
                } else {
                    self.state.lock().unwrap().phase = Phase::Ready;
                }
                return Err(error);
            }
        };
        let cleanup = previous.shutdown().await.err().unwrap_or_default();
        let outcome = ApplyOutcome {
            configuration_revision: next.revision(),
            applied: true,
            ready: cleanup.is_empty(),
            cleanup_errors: cleanup.clone(),
            persistence_warning,
        };
        let mut state = self.state.lock().unwrap();
        state.runtime = next;
        state.phase = if cleanup.is_empty() {
            Phase::Ready
        } else {
            Phase::Blocked
        };
        state.cleanup = cleanup;
        state.persistence_warning = persistence_warning;
        Ok(outcome)
    }
    async fn prepare_and_commit(
        &self,
        expected: Uuid,
        candidate: HubConfig,
    ) -> Result<(Arc<HubRuntime>, Option<&'static str>), UpdateError> {
        let store = self.store.as_ref().expect("Writable service").clone();
        let prepared =
            tokio::task::spawn_blocking(move || store.prepare(expected, candidate, false))
                .await
                .map_err(|_| UpdateError::Task)??;
        // Credential resolution may touch disk/DPAPI. Keep preparation off the
        // async executor, while still constructing actors in this Tokio runtime.
        let builder = self.builder.as_ref().expect("Writable service").clone();
        let candidate = prepared.configuration().clone();
        let next = tokio::task::spawn_blocking(move || build_checked(&builder, &candidate))
            .await
            .map_err(|_| UpdateError::Task)?
            .map_err(UpdateError::Invalid)?;
        let store = self.store.as_ref().unwrap().clone();
        let committed = tokio::task::spawn_blocking(move || store.commit(prepared)).await;
        match committed {
            Ok(Ok(_)) => Ok((next, None)),
            Ok(Err(ApplyError::Committed { .. })) => Ok((
                next,
                Some(
                    "Configuration was replaced, but final filesystem durability could not be confirmed",
                ),
            )),
            result => {
                let _ = next.shutdown().await;
                Err(match result {
                    Ok(Err(error)) => error.into(),
                    Err(_) => UpdateError::Task,
                    Ok(Ok(_)) => unreachable!(),
                })
            }
        }
    }
    pub async fn shutdown(&self) -> Result<(), Vec<(Uuid, SourceError)>> {
        // An accepted apply finishes its commit/drain before ownership can end.
        let _guard = self.update.lock().await;
        let (runtime, mut cleanup) = {
            let mut state = self.state.lock().unwrap();
            state.phase = Phase::Stopped;
            (state.runtime.clone(), state.cleanup.clone())
        };
        cleanup.extend(runtime.shutdown().await.err().unwrap_or_default());
        if cleanup.is_empty() {
            Ok(())
        } else {
            Err(cleanup)
        }
    }
}

fn build_checked(
    builder: &Arc<RuntimeBuilder>,
    config: &HubConfig,
) -> Result<Arc<HubRuntime>, Vec<FieldError>> {
    let runtime = builder(config.clone())?;
    if runtime.configuration() != config {
        return Err(vec![FieldError::new(
            "",
            "revision",
            "Prepared runtime does not match the candidate configuration",
        )]);
    }
    Ok(runtime)
}

/// A stream keeps its client identity across configuration revisions. Rebinding
/// is only possible after apply proved all output connections were released.
pub struct ServiceClient {
    id: Uuid,
    state: Mutex<ClientBinding>,
}
struct ClientBinding {
    closed: bool,
    client: Option<Arc<ClientSession>>,
}
impl Default for ServiceClient {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            state: Mutex::new(ClientBinding {
                closed: false,
                client: None,
            }),
        }
    }
}
impl ServiceClient {
    pub fn id(&self) -> Uuid {
        self.id
    }
    pub fn bind(&self, runtime: &Arc<HubRuntime>) -> Result<Arc<ClientSession>, SourceError> {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Err(SourceError::new(
                ErrorKind::Disconnected,
                "Local IPC client closed",
            ));
        }
        if state
            .client
            .as_ref()
            .is_none_or(|client| client.runtime_id() != runtime.runtime_id())
        {
            if let Some(previous) = state.client.take() {
                previous.close();
            }
            state.client = Some(runtime.client_with_id(self.id));
        }
        Ok(state.client.as_ref().unwrap().clone())
    }
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        if let Some(client) = state.client.take() {
            client.close();
        }
    }
}
impl Drop for ServiceClient {
    fn drop(&mut self) {
        self.close();
    }
}
