use anyhow::{Context, Result, ensure};
use std::{path::PathBuf, sync::Arc};
use zwogain_alpaca::{profile::Profiles, server::Log};
use zwogain_core::Runtime;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut profile = None;
    let mut simulate = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--profiles" => {
                profile = Some(PathBuf::from(args.next().context("Missing profiles path")?))
            }
            "--simulate" => simulate = true,
            _ => anyhow::bail!("Unknown option: {arg}"),
        }
    }
    let path = profile.context("--profiles PATH is required")?;
    ensure!(path.is_absolute(), "Use an absolute profile path");
    let directory = std::env::current_exe()?.parent().unwrap().to_path_buf();
    let runtime = Runtime {
        sdk: directory.join(if cfg!(windows) {
            "ASICamera2.dll"
        } else if cfg!(target_os = "macos") {
            "libASICamera2.dylib"
        } else {
            "libASICamera2.so"
        }),
        directory,
        simulate,
        sdk_simulation: None,
    };
    let log = Log::new(Some(path.parent().unwrap().join("logs")));
    zwogain_alpaca::native::run(runtime, Arc::new(Profiles::new(Some(path))?), log).await
}
