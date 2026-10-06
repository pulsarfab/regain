//! Stable switch slots and source-authorized writes. Scalar gauges are read-only.
use crate::{
    config::{HubConfig, Readout, SwitchChannel, VirtualDevice},
    readout::{SourceLease, invalid, scalar, unavailable},
    safety::Clock,
    source::{ErrorKind, SourceError, SourceRegistry, Values},
};
use serde::Serialize;
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Grid {
    pub minimum: f64,
    pub maximum: f64,
    pub step: f64,
}
impl Grid {
    pub fn new(minimum: f64, maximum: f64, step: f64) -> Result<Self, SourceError> {
        let steps = (maximum - minimum) / step;
        if !minimum.is_finite()
            || !maximum.is_finite()
            || !step.is_finite()
            || minimum >= maximum
            || step <= 0.0
            || !steps.is_finite()
            || !(1.0..=4503599627370496.0).contains(&steps)
            || !near(steps, steps.round())
        {
            return Err(invalid(
                "Switch bounds must span a whole, representable number of positive steps",
            ));
        }
        Ok(Self {
            minimum,
            maximum,
            step,
        })
    }
    /// ASCOM rounds an in-range value to the nearest supported step.
    pub fn quantize(&self, value: f64) -> Result<f64, SourceError> {
        if !value.is_finite() || value < self.minimum || value > self.maximum {
            return Err(invalid("Switch value is outside its range"));
        }
        Ok(
            (self.minimum + ((value - self.minimum) / self.step).round() * self.step)
                .clamp(self.minimum, self.maximum),
        )
    }
    pub fn supports(&self, exposed: &Self) -> bool {
        exposed.minimum >= self.minimum
            && exposed.maximum <= self.maximum
            && near(
                (exposed.minimum - self.minimum) / self.step,
                ((exposed.minimum - self.minimum) / self.step).round(),
            )
            && near(exposed.step / self.step, (exposed.step / self.step).round())
            && exposed.step >= self.step
    }
}
fn near(a: f64, b: f64) -> bool {
    (a - b).abs() <= 8.0 * f64::EPSILON * a.abs().max(b.abs()).max(1.0)
}

pub struct SwitchOutput {
    channels: BTreeMap<u32, SwitchChannel>,
    slots: u32,
    registry: Arc<SourceRegistry>,
    clock: Arc<dyn Clock>,
    maximum_age: BTreeMap<Uuid, f64>,
}
impl SwitchOutput {
    pub fn new(
        config: &HubConfig,
        output: Uuid,
        registry: Arc<SourceRegistry>,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<Self>, SourceError> {
        if !config.validate().is_empty() {
            return Err(invalid("Invalid hub configuration"));
        }
        let Some(VirtualDevice::Switch { channels }) = config
            .outputs
            .iter()
            .find(|item| item.id == output)
            .map(|item| &item.device)
        else {
            return Err(invalid("Expected a switch output"));
        };
        let maximum_age = config
            .sources
            .iter()
            .map(|source| {
                (
                    source.id,
                    source.polling.poll_seconds + source.polling.request_timeout_seconds,
                )
            })
            .collect();
        Ok(Arc::new(Self {
            channels: channels
                .iter()
                .map(|channel| (channel.number, channel.clone()))
                .collect(),
            slots: config.switch_slot_count(output),
            registry,
            clock,
            maximum_age,
        }))
    }
    pub async fn connect(self: &Arc<Self>) -> Result<SwitchSession, SourceError> {
        let mut leases = BTreeMap::new();
        for channel in self.channels.values() {
            let id = channel.readout.source();
            if let std::collections::btree_map::Entry::Vacant(entry) = leases.entry(id) {
                entry.insert(SourceLease::acquire(self.registry.get(id)?).await?);
            }
        }
        Ok(SwitchSession {
            output: self.clone(),
            leases,
        })
    }
    pub fn max_switch(&self) -> u32 {
        self.slots
    }
    pub fn channel(&self, number: u32) -> Result<Option<&SwitchChannel>, SourceError> {
        if number >= self.slots {
            return Err(invalid("Switch channel is outside the device's slot range"));
        }
        Ok(self.channels.get(&number))
    }
    pub fn name(&self, number: u32) -> Result<String, SourceError> {
        Ok(self
            .channel(number)?
            .map(|channel| channel.label.clone())
            .unwrap_or_else(|| format!("Removed channel {number}")))
    }
}
pub struct SwitchSession {
    output: Arc<SwitchOutput>,
    leases: BTreeMap<Uuid, SourceLease>,
}
impl SwitchSession {
    fn active(&self, number: u32) -> Result<&SwitchChannel, SourceError> {
        self.output
            .channel(number)?
            .ok_or_else(|| unavailable("This channel was removed and its number is reserved"))
    }
    pub fn value(&self, number: u32) -> Result<f64, SourceError> {
        let channel = self.active(number)?;
        let sample = scalar(
            &self.leases[&channel.readout.source()].source.snapshot(),
            &channel.readout,
            self.output.clock.now(),
        )?;
        if sample.age_seconds >= self.output.maximum_age[&sample.source] {
            return Err(unavailable("Switch reading is stale"));
        }
        if sample.value < channel.minimum || sample.value > channel.maximum {
            return Err(unavailable("Switch reading is outside its declared bounds"));
        }
        Ok(sample.value)
    }
    pub fn state(&self, number: u32) -> Result<bool, SourceError> {
        Ok(self.value(number)? != self.active(number)?.minimum)
    }
    pub async fn can_write(&self, number: u32) -> Result<bool, SourceError> {
        let Some(channel) = self.output.channel(number)? else {
            return Ok(false);
        };
        if !channel.writable || !matches!(channel.readout, Readout::Channel { .. }) {
            return Ok(false);
        }
        Ok(self.upstream_grid(channel).await?.is_some())
    }
    async fn upstream_grid(&self, channel: &SwitchChannel) -> Result<Option<Grid>, SourceError> {
        let Readout::Channel {
            source,
            channel: id,
            ..
        } = channel.readout
        else {
            return Ok(None);
        };
        let lease = &self.leases[&source];
        let initial = lease.source.snapshot().generation;
        let args = Values::from([("Id".into(), json!(id))]);
        let writable = lease
            .source
            .read(lease.id, "canwrite", args.clone())
            .await?
            .as_bool()
            .ok_or_else(|| unavailable("Source returned invalid write capabilities"))?;
        if !writable {
            return Ok(None);
        }
        let mut bounds = Vec::new();
        for member in ["minswitchvalue", "maxswitchvalue", "switchstep"] {
            bounds.push(
                lease
                    .source
                    .read(lease.id, member, args.clone())
                    .await?
                    .as_f64()
                    .ok_or_else(|| unavailable("Source returned invalid switch bounds"))?,
            );
        }
        if lease.source.snapshot().generation != initial {
            return Err(unavailable("Source reconnected while reading capabilities"));
        }
        let grid = Grid::new(bounds[0], bounds[1], bounds[2])?;
        if !grid.supports(&Grid::new(channel.minimum, channel.maximum, channel.step)?) {
            return Err(invalid(
                "Configured switch bounds or step do not match the source",
            ));
        }
        Ok(Some(grid))
    }
    pub async fn set_state(&self, number: u32, value: bool) -> Result<(), SourceError> {
        let channel = self.active(number)?;
        self.set_value(
            number,
            if value {
                channel.maximum
            } else {
                channel.minimum
            },
        )
        .await
    }
    pub async fn set_value(&self, number: u32, value: f64) -> Result<(), SourceError> {
        let channel = self.active(number)?;
        if !channel.writable {
            return Err(SourceError::new(
                ErrorKind::Unsupported,
                "This channel is read-only",
            ));
        }
        let value = Grid::new(channel.minimum, channel.maximum, channel.step)?.quantize(value)?;
        let Readout::Channel {
            source,
            channel: id,
            ..
        } = channel.readout
        else {
            return Err(SourceError::new(
                ErrorKind::Unsupported,
                "Scalar property gauges are read-only",
            ));
        };
        // Unique control owner per operation, including concurrent commands from
        // one frontend. The guard releases ownership if its future is cancelled.
        if self.leases[&source].source.snapshot().write_uncertain {
            return Err(SourceError::uncertain());
        }
        let operation = SourceLease::acquire(self.leases[&source].source.clone()).await?;
        operation.source.control(operation.id, true).await?;
        let generation = operation.source.snapshot().generation;
        if self.upstream_grid(channel).await?.is_none() {
            return Err(SourceError::new(
                ErrorKind::Unsupported,
                "The source channel is read-only",
            ));
        }
        if generation != operation.source.snapshot().generation {
            return Err(unavailable("Source reconnected before the switch write"));
        }
        operation
            .source
            .write_fenced(
                operation.id,
                "setswitchvalue",
                Values::from([("Id".into(), json!(id)), ("Value".into(), json!(value))]),
                Some(generation),
            )
            .await?;
        Ok(())
    }
}
