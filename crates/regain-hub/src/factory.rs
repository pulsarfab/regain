//! Configuration-derived source construction. Every output contributes to one
//! poll plan per source; construction performs no device I/O or discovery.
use crate::{
    alpaca::{AlpacaBackend, SampleRequest},
    config::{DeviceType, HubConfig, Readout, SourceBackend, SourceConfig, VirtualDevice},
    native::{NativeAccessoryBackend, NativeRuntime},
    parameters::FieldError,
    safety::Clock,
    source::{ErrorKind, MAX_SAMPLE_KEYS, SourceError, SourceRegistry},
};
use reqwest::header::HeaderValue;
use std::{collections::BTreeMap, sync::Arc};
use uuid::Uuid;

/// The host resolves references from user-protected storage. Implementations
/// must return sanitized errors and must not log or persist the returned value.
pub trait CredentialProvider {
    fn authorization(&self, reference: &str) -> Result<HeaderValue, SourceError>;
}

/// Explicitly unavailable until a host supplies its protected credential store.
pub struct NoCredentials;
impl CredentialProvider for NoCredentials {
    fn authorization(&self, _: &str) -> Result<HeaderValue, SourceError> {
        Err(SourceError::new(
            ErrorKind::Unavailable,
            "A protected credential provider is required for this source",
        ))
    }
}

pub struct SourcePlan {
    /// Effective polling settings; persisted configuration remains unchanged.
    pub source: SourceConfig,
    pub samples: Vec<SampleRequest>,
}

pub fn source_plans(config: &HubConfig) -> Result<BTreeMap<Uuid, SourcePlan>, Vec<FieldError>> {
    let errors = config.validate();
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut plans = BTreeMap::new();
    for (index, source) in config.sources.iter().enumerate() {
        let device_type = config
            .source_type(source.id)
            .expect("Validated source type");
        let mut effective = source.clone();
        let mut samples = BTreeMap::new();
        let mut add = |readout: &Readout| -> Result<(), Vec<FieldError>> {
            if readout.source() == source.id {
                let sample =
                    SampleRequest::readout(readout, device_type == DeviceType::ObservingConditions);
                samples.entry(sample.key.clone()).or_insert(sample);
                if samples.len() > MAX_SAMPLE_KEYS {
                    return Err(vec![FieldError::new(
                        format!("sources[{index}]"),
                        "sample",
                        "Combined source poll plan exceeds the sample limit",
                    )]);
                }
            }
            Ok(())
        };
        for output in &config.outputs {
            match &output.device {
                VirtualDevice::Safety { members } => {
                    for member in members
                        .iter()
                        .filter(|m| m.enabled && m.source == source.id)
                    {
                        effective.polling.poll_seconds = effective
                            .polling
                            .poll_seconds
                            .min(member.policy.confirmation_seconds);
                    }
                }
                VirtualDevice::Switch { channels } => {
                    for channel in channels {
                        add(&channel.readout)?;
                    }
                }
                VirtualDevice::Weather { measurements } => {
                    for readout in measurements.values().flat_map(|m| &m.sources) {
                        add(readout)?;
                    }
                }
                VirtualDevice::Proxy { .. } => {}
            }
        }
        if device_type == DeviceType::SafetyMonitor {
            // Safety events must contain exactly one strict boolean IsSafe.
            // Never bundle unrelated properties into a safety attempt.
            if samples.keys().any(|key| key != "issafe") {
                return Err(vec![FieldError::new(
                    format!("sources[{index}]"),
                    "sample",
                    "SafetyMonitor scalar mappings must select IsSafe",
                )]);
            }
            samples.insert("issafe".into(), SampleRequest::safety());
        }
        plans.insert(
            source.id,
            SourcePlan {
                source: effective,
                samples: samples.into_values().collect(),
            },
        );
    }
    Ok(plans)
}

/// Prepare every transport and resolve every credential before spawning actors.
/// Unsupported adapters fail explicitly; they never fall back to simulation.
/// The caller retains this registry for the applied configuration revision.
pub fn build_sources(
    config: &HubConfig,
    native: &NativeRuntime,
    credentials: &dyn CredentialProvider,
    clock: Arc<dyn Clock>,
) -> Result<Arc<SourceRegistry>, Vec<FieldError>> {
    build_sources_bound(config, native, credentials, clock, None)
}

pub(crate) fn build_sources_bound(
    config: &HubConfig,
    native: &NativeRuntime,
    credentials: &dyn CredentialProvider,
    clock: Arc<dyn Clock>,
    binding: Option<crate::virtual_source::Binding>,
) -> Result<Arc<SourceRegistry>, Vec<FieldError>> {
    let mut plans = source_plans(config)?;
    let simulated = simulated_sources(config, native.simulate);
    let mut effective = config.clone();
    for source in &mut effective.sources {
        source.polling = plans[&source.id].source.polling.clone();
    }
    SourceRegistry::build(&effective, clock.clone(), |source| {
        let plan = plans.remove(&source.id).expect("Validated source plan");
        match &source.backend {
            SourceBackend::Native { .. } => Ok(Box::new(NativeAccessoryBackend::new(
                source,
                native.clone(),
            )?)),
            SourceBackend::Alpaca {
                credential_reference,
                ..
            } => {
                let authorization = credential_reference
                    .as_deref()
                    .map(|reference| credentials.authorization(reference))
                    .transpose()?;
                Ok(Box::new(AlpacaBackend::new(
                    source,
                    plan.samples,
                    authorization,
                )?))
            }
            SourceBackend::Com { .. } => {
                let mut backend = crate::com::ComBackend::new(source, native, plan.samples)?;
                backend.exclude_exports(crate::ascom_export::classes(config));
                Ok(Box::new(backend))
            }
            SourceBackend::Virtual { output } => {
                let binding = binding.clone().ok_or_else(|| {
                    SourceError::new(
                        ErrorKind::Unsupported,
                        "Virtual source construction requires the shared output runtime",
                    )
                })?;
                Ok(Box::new(crate::virtual_source::VirtualBackend::new(
                    binding,
                    *output,
                    config.source_type(source.id).expect("Validated type"),
                    plan.samples,
                    clock.clone(),
                    simulated.contains(&source.id),
                )))
            }
            SourceBackend::Simulated { device_type } => Ok(Box::new(
                crate::simulated::SimulatedBackend::new(*device_type, plan.samples)?,
            )),
        }
    })
    .map(Arc::new)
}

// Propagate once per graph level, rather than recursively traversing shared
// subgraphs exponentially. Validation has already rejected cycles/missing nodes.
fn simulated_sources(
    config: &HubConfig,
    native_simulated: bool,
) -> std::collections::BTreeSet<Uuid> {
    let mut marked: std::collections::BTreeSet<_> = config
        .sources
        .iter()
        .filter(|s| {
            matches!(s.backend, SourceBackend::Simulated { .. })
                || native_simulated && matches!(s.backend, SourceBackend::Native { .. })
        })
        .map(|s| s.id)
        .collect();
    let dependencies: BTreeMap<_, _> = config
        .outputs
        .iter()
        .map(|o| (o.id, o.device.sources()))
        .collect();
    loop {
        let previous = marked.len();
        for source in &config.sources {
            if let SourceBackend::Virtual { output } = source.backend
                && dependencies[&output].iter().any(|id| marked.contains(id))
            {
                marked.insert(source.id);
            }
        }
        if marked.len() == previous {
            return marked;
        }
    }
}
