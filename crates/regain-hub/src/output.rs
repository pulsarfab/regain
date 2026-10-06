//! Shared typed scalar operations for local composition and frontend IPC.
use crate::{
    ipc::{Get, Put},
    runtime::OutputConnection,
    source::SourceError,
};
use serde_json::{Value, json};
impl OutputConnection {
    pub(crate) async fn get(&self, property: Get) -> Result<Value, SourceError> {
        Ok(match property {
            Get::Connected {} => json!(true),
            Get::IsSafe {} => json!(self.safety()?.snapshot().is_safe),
            Get::SafetyStatus {} => json!(self.safety()?.snapshot()),
            Get::MaxSwitch {} => json!(self.switch_definition()?.max_switch()),
            Get::GetSwitch { id } => json!(self.switch()?.state(id)?),
            Get::GetSwitchValue { id } => json!(self.switch()?.value(id)?),
            Get::GetSwitchName { id } => json!(self.switch_definition()?.name(id)?),
            Get::GetSwitchDescription { id } => {
                let channel = self.switch_definition()?.channel(id)?;
                json!(
                    channel
                        .map(|c| format!("{}; source {}", c.units, c.readout.source()))
                        .unwrap_or_else(|| "Removed channel".into())
                )
            }
            Get::CanWrite { id } => json!(self.switch()?.can_write(id).await?),
            Get::MinSwitchValue { id } => json!(
                self.switch_definition()?
                    .channel(id)?
                    .map_or(0.0, |c| c.minimum)
            ),
            Get::MaxSwitchValue { id } => json!(
                self.switch_definition()?
                    .channel(id)?
                    .map_or(1.0, |c| c.maximum)
            ),
            Get::SwitchStep { id } => json!(
                self.switch_definition()?
                    .channel(id)?
                    .map_or(1.0, |c| c.step)
            ),
            Get::Measurement { metric } => json!(self.weather()?.read(metric)?),
            Get::TimeSinceLastUpdate { sensor } => {
                json!(self.weather()?.time_since_last_update(&sensor)?)
            }
            Get::AveragePeriod {} => json!(self.weather()?.average_period_hours()),
            Get::SensorDescription { sensor } => {
                json!(self.weather()?.sensor_description(&sensor)?)
            }
        })
    }
    pub(crate) async fn put(&self, property: Put) -> Result<(), SourceError> {
        match property {
            Put::SetSwitch { id, state } => self.switch()?.set_state(id, state).await?,
            Put::SetSwitchValue { id, value } => self.switch()?.set_value(id, value).await?,
            Put::AveragePeriod { hours } => self.weather()?.set_average_period_hours(hours)?,
            Put::Refresh {} => self.weather()?.refresh().await?,
        }
        Ok(())
    }
}
