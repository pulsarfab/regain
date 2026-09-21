use anyhow::{Result, bail};
fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let vendor = args.next().unwrap_or_default();
    if vendor.is_empty() || vendor == "--help" || vendor == "help" {
        println!(
            "PulsarFab regain device worker\nUsage: regain-device VENDOR DEVICE [arguments]\n\nZWO:        zwo camera-direct | camera-sdk | caa | efw | eaf\nPegasus:    pegasus fc3\nDeepSkyDad: deepskydad ofp2\nWanderer:   wanderer eta\n\nAccessory commands: list-details | status | serve [--serial ID] [--simulate]\nCamera commands: --help"
        );
        return Ok(());
    }
    let device = args.next().unwrap_or_default();
    let args: Vec<String> = args.collect();
    match (vendor.as_str(), device.as_str()) {
        ("zwo", "camera-direct") => regain_zwo::asi::direct::run(args),
        ("zwo", "camera-sdk") => regain_zwo::asi::sdk::run(args),
        ("zwo", "caa") => regain_zwo::caa::worker::run(args),
        ("zwo", "efw" | "eaf") => {
            regain_zwo::accessories::worker::run(std::iter::once(device).chain(args).collect())
        }
        ("pegasus", "fc3") => regain_pegasus::fc3::worker::run(args),
        ("deepskydad", "ofp2") => regain_deepskydad::ofp2::worker::run(args),
        ("wanderer", "eta") => regain_wanderer::eta::worker::run(args),
        _ => {
            bail!("Unknown device: {vendor} {device}; see regain-device --help")
        }
    }
}
