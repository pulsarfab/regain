//! Device-scoped USB recovery. Vendor code binds a serial to a physical device;
//! this module never selects the first device, resets a hub, or runs a shell.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Target {
    pub vendor: u16,
    pub product: u16,
    pub serial: String,
    pub location: String,
    // Linux device address detects replacement/reenumeration since binding.
    pub address: Option<u8>,
    pub generation: Option<u64>,
}
impl Target {
    #[cfg(windows)]
    pub fn bind_generation(&mut self) -> Result<()> {
        self.generation = Some(windows::generation(&self.location)?);
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.vendor == 0x03c3
                && matches!(
                    self.product,
                    0x585e | 0x676d | 0x662b | 0x2601 | 0x2209 | 0x620b | 0x260e
                ),
            "USB recovery is limited to cameras with verified serial binding"
        );
        ensure!(
            self.serial.len() == 16
                && self.serial.bytes().all(|c| c.is_ascii_hexdigit())
                && self.serial != "0000000000000000",
            "Invalid camera serial for USB recovery"
        );
        ensure!(
            !self.location.is_empty()
                && self.location.len() <= 512
                && self
                    .location
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"\\&_.-".contains(&c)),
            "Invalid USB recovery location"
        );
        Ok(())
    }
    pub fn encode(&self) -> Result<String> {
        self.validate()?;
        Ok(serde_json::to_vec(self)?
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect())
    }
    pub fn decode(token: &str) -> Result<Self> {
        ensure!(
            token.len() <= 4096
                && token.len().is_multiple_of(2)
                && token.bytes().all(|c| c.is_ascii_hexdigit()),
            "Invalid USB recovery token"
        );
        let bytes = (0..token.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&token[i..i + 2], 16))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let target: Self = serde_json::from_slice(&bytes)?;
        target.validate()?;
        Ok(target)
    }
}

/// Isolated helper with a hard lifetime bound. Cancellation by the supervisor
/// waits for this operation to finish so a port cycle can restore its port.
pub fn run(args: Vec<String>) -> Result<()> {
    ensure!(
        args.len() == 2 || args.len() == 4,
        "Usage: regain-device usb reset|cycle TARGET [--elevated DEADLINE]"
    );
    let cycle = match args[0].as_str() {
        "reset" => false,
        "cycle" => true,
        _ => anyhow::bail!("Unknown USB recovery operation"),
    };
    let target = Target::decode(&args[1])?;
    #[cfg(windows)]
    {
        windows::run(&target, cycle, &args)
    }
    #[cfg(target_os = "linux")]
    {
        ensure!(args.len() == 2, "Elevation flags are Windows-only");
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(60));
            std::process::exit(124);
        });
        linux::recover(&target, cycle)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (target, cycle);
        anyhow::bail!("USB recovery is supported on Windows and Linux only")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn token_rejects_hubs_paths_and_arbitrary_commands() {
        let mut t = Target {
            vendor: 0x03c3,
            product: 0x676d,
            serial: "1234567890abcdef".into(),
            location: "1-2.3".into(),
            address: Some(2),
            generation: None,
        };
        assert_eq!(
            Target::decode(&t.encode().unwrap()).unwrap().serial,
            t.serial
        );
        t.product = 0;
        assert!(t.validate().is_err());
        t.product = 0x676d;
        t.location = "../../sys".into();
        assert!(t.validate().is_err());
        assert!(Target::decode("not a command").is_err());
    }
}
