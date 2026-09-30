use super::Target;
use anyhow::{Context, Result, ensure};
use nusb::MaybeFuture;
use std::{path::Path, time::Duration};

pub fn recover(target: &Target, cycle: bool) -> Result<()> {
    // Match the physical path AND the enumeration generation, never a bus address alone.
    let matches: Vec<_> = nusb::list_devices()
        .wait()?
        .filter(|d| {
            d.vendor_id() == target.vendor
                && d.product_id() == target.product
                && d.sysfs_path().file_name().and_then(|s| s.to_str())
                    == Some(target.location.as_str())
                && Some(d.device_address()) == target.address
        })
        .collect();
    ensure!(
        matches.len() == 1,
        "Bound USB camera is missing or has reenumerated; reconnect before recovery"
    );
    let info = &matches[0];
    ensure!(info.class() != 9, "Refusing to reset a USB hub");
    let device = info
        .open()
        .wait()
        .context("Open bound USB camera; grant device access with a scoped udev rule")?;
    let config = device
        .active_configuration()
        .context("Camera has no active configuration")?;
    let _claims = config
        .interfaces()
        .map(|i| {
            device
                .claim_interface(i.interface_number())
                .wait()
                .context("USB camera is owned by another driver or application")
        })
        .collect::<Result<Vec<_>>>()?;
    if !cycle {
        return device.reset().wait().context("Reset bound USB device");
    }
    let path = info.sysfs_path().canonicalize()?;
    let port = *info
        .port_chain()
        .last()
        .context("Camera is not on a downstream port")?;
    let parent = path.parent().context("No parent USB hub")?;
    let hub = parent
        .file_name()
        .and_then(|s| s.to_str())
        .context("Invalid hub path")?;
    // The port-device link lives beneath the hub interface (USB 2 and USB 3).
    let prefix = if let Some(bus) = hub.strip_prefix("usb") {
        format!("{bus}-0:")
    } else {
        format!("{hub}:")
    };
    let mut ports = Vec::new();
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let disable = entry.path().join(format!("{hub}-port{port}/disable"));
            if disable.exists() {
                ports.push(disable);
            }
        }
    }
    ensure!(
        ports.len() == 1,
        "No unambiguous Linux 6.0+ port disable control for this camera"
    );
    cycle_port(&ports[0])
}

fn cycle_port(path: &Path) -> Result<()> {
    ensure!(
        std::fs::read_to_string(path)?.trim() == "0",
        "USB port is already disabled"
    );
    // Open the restore descriptor before changing anything. Keep it valid even
    // when disabling the child removes that child's sysfs directory.
    use std::io::{Seek, Write};
    let mut restore = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .context("Port cycling needs root or scoped permissions on the hub port's disable file")?;
    struct Restore<'a>(&'a mut std::fs::File);
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            let _ = self.0.rewind();
            let _ = self.0.write_all(b"0");
        }
    }
    let guard = Restore(&mut restore);
    std::fs::write(path, b"1").context("Disable camera USB port")?;
    std::thread::sleep(Duration::from_secs(2));
    guard.0.write_all(b"0").context("Restore camera USB port")?;
    // Avoid a second sysfs write with a nonzero file offset.
    std::mem::forget(guard);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cycle_restores_port_and_refuses_already_disabled_port() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disable");
        std::fs::write(&path, "0").unwrap();
        cycle_port(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "0");
        std::fs::write(&path, "1").unwrap();
        assert!(cycle_port(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "1");
    }
}
