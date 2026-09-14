//! Research executable; never loaded into NINA or included in the plugin ZIP.
mod protocol;
#[cfg(windows)]
mod transport;
use anyhow::{Result, ensure};

#[cfg(windows)]
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.is_empty() || args == ["--probe"] || args == ["--probe", "--cancel-read"],
        "Usage: zwogain-direct [--probe [--cancel-read]]; disconnect other camera apps first"
    );
    // Last resort for a kernel request that refuses to finish cancellation. The
    // worker must exit rather than free a buffer still owned by the USB driver.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(30));
        eprintln!("Direct-driver worker deadline expired");
        std::process::exit(124);
    });
    let paths = transport::enumerate()?;
    if args.is_empty() {
        println!(
            "{}",
            serde_json::json!({"sdkLoaded":false,"interfaces":paths.len()})
        );
        return Ok(());
    }
    ensure!(
        paths.len() == 1,
        "probe requires exactly one attached ZWO driver interface"
    );
    let camera = transport::Camera::open(&paths[0])?;
    let mut result = camera.probe()?;
    if args.len() == 2 {
        ensure!(
            result["endpoints"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["address"] == 0x81 && e["attributes"] == 2),
            "observed bulk-IN endpoint 0x81 is absent"
        );
        result["bulkRead"] = camera.cancel_read()?;
    }
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

#[cfg(not(windows))]
fn main() -> Result<()> {
    anyhow::bail!("The direct-driver research executable requires Windows");
}
