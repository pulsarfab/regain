use anyhow::{Result, bail};
fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let vendor = args.next().unwrap_or_default();
    if vendor == "--hub-discover" {
        // No SDK/device code runs until the parent has attached process ownership.
        // Fixed argv selects one accessory kind; no arbitrary command is accepted.
        let vendor = args.next().unwrap_or_default();
        let device = args.next().unwrap_or_default();
        let remaining: Vec<_> = args.collect();
        anyhow::ensure!(
            remaining.is_empty() || remaining == ["--simulate"],
            "Invalid discovery arguments"
        );
        anyhow::ensure!(
            matches!(
                (vendor.as_str(), device.as_str()),
                ("zwo", "caa" | "efw" | "eaf")
                    | ("pegasus", "fc3" | "falcon")
                    | ("deepskydad", "ofp2")
                    | ("wanderer", "eta")
            ),
            "Unsupported discovery kind"
        );
        use std::io::{BufRead, Read};
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .take(4096)
            .read_until(b'\n', &mut bytes)?;
        anyhow::ensure!(
            bytes == b"{\"command\":\"discover\"}\n",
            "Invalid discovery trigger"
        );
        let mut discovery = vec!["list-details".to_owned()];
        discovery.extend(remaining);
        return dispatch(&vendor, &device, discovery);
    }
    if vendor == "usb" {
        return regain_transport::usb::run(args.collect());
    }
    if vendor.is_empty() || vendor == "--help" || vendor == "help" {
        println!(
            "PulsarFab regain device worker\nUsage: regain-device VENDOR DEVICE [arguments]\n\nZWO:        zwo camera-direct | camera-sdk | caa | efw | eaf\nPegasus:    pegasus fc3 | falcon\nDeepSkyDad: deepskydad ofp2\nWanderer:   wanderer eta\n\nAccessory commands: list-details | status | serve [--serial ID] [--simulate]\nUSB recovery: usb reset|cycle TARGET (see docs/usb-recovery.md)\nCamera commands: --help"
        );
        return Ok(());
    }
    let device = args.next().unwrap_or_default();
    let args: Vec<String> = args.collect();
    dispatch(&vendor, &device, args)
}
fn dispatch(vendor: &str, device: &str, args: Vec<String>) -> Result<()> {
    match (vendor, device) {
        ("zwo", "camera-direct") => regain_zwo::asi::direct::run(args),
        ("zwo", "camera-sdk") => regain_zwo::asi::sdk::run(args),
        ("zwo", "caa") => regain_zwo::caa::worker::run(args),
        ("zwo", "efw" | "eaf") => regain_zwo::accessories::worker::run(
            std::iter::once(device.to_owned()).chain(args).collect(),
        ),
        ("pegasus", "fc3") => regain_pegasus::fc3::worker::run(args),
        ("pegasus", "falcon") => regain_pegasus::falcon::worker::run(args),
        ("deepskydad", "ofp2") => regain_deepskydad::ofp2::worker::run(args),
        ("wanderer", "eta") => regain_wanderer::eta::worker::run(args),
        _ => {
            bail!("Unknown device: {vendor} {device}; see regain-device --help")
        }
    }
}
