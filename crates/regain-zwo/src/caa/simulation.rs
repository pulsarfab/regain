//! Explicit opt-in HID simulator; exercises the production motion coordinator.
use crate::caa::Transport;
use anyhow::Result;

pub struct Sim {
    query: u8,
    position: u32,
    limit: u16,
    reverse: bool,
    beep: bool,
    alias: [u8; 8],
}
impl Default for Sim {
    fn default() -> Self {
        Self {
            query: 0,
            position: 1_520_000,
            limit: 360,
            reverse: false,
            beep: true,
            alias: [0; 8],
        }
    }
}
impl Transport for Sim {
    fn set_output(&mut self, r: &[u8]) -> Result<()> {
        match r[3] {
            2 => self.query = r[4],
            9 => self.reverse = r[4] != 0,
            7 => self.beep = r[4] != 0,
            13 => self.alias.copy_from_slice(&r[4..12]),
            3 if r[4] == 1 || r[10] == 1 => {
                self.position = u32::from_be_bytes(r[6..10].try_into().unwrap())
            }
            3 if r[10] == 2 => self.limit = u16::from_be_bytes([r[14], r[15]]),
            _ => (),
        }
        Ok(())
    }
    fn get_input(&mut self) -> Result<Vec<u8>> {
        let mut r = vec![0; 18];
        r[..4].copy_from_slice(&[1, 126, 90, self.query]);
        match self.query {
            3 => {
                r[6..10].copy_from_slice(&self.position.to_be_bytes());
                r[13..15].copy_from_slice(&self.limit.to_be_bytes());
            }
            4 => {
                r[4..7].copy_from_slice(&[1, 1, 1]);
                r[8..15].copy_from_slice(b"CAA-SIM");
            }
            8 => {
                r[4] = self.beep as u8;
                r[5] = self.reverse as u8;
            }
            12 => r[4..12].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]),
            13 => r[4..12].copy_from_slice(&self.alias),
            _ => (),
        }
        Ok(r)
    }
}
