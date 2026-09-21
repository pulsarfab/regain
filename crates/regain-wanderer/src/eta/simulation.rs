use crate::eta::{Telemetry, Transport, command};
use anyhow::Result;
pub const SERIAL: &str = "SIMULATION";
pub struct Simulation {
    positions: [f64; 3],
    pub sent: Vec<(usize, i32)>,
}
impl Default for Simulation {
    fn default() -> Self {
        Self {
            positions: [400., 410., 420.],
            sent: vec![],
        }
    }
}
impl Transport for Simulation {
    fn read(&mut self) -> Result<Telemetry> {
        Ok(Telemetry {
            firmware: "20260804".into(),
            points_um: self.positions,
            extra: vec!["1".into()],
        })
    }
    fn send(&mut self, point: usize, target_um: i32) -> Result<()> {
        command(point, target_um)?;
        self.sent.push((point, target_um));
        self.positions[point - 1] = target_um as f64;
        Ok(())
    }
}
