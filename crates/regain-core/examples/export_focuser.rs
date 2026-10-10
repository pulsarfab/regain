fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("Usage: export_focuser PATH [--check]"))?;
    let check = args.next();
    anyhow::ensure!(
        check.as_deref().is_none_or(|v| v == "--check") && args.next().is_none(),
        "Unknown option"
    );
    let bytes = serde_json::to_string_pretty(&regain_core::focuser::schema())? + "\n";
    if check.is_some() {
        anyhow::ensure!(
            std::fs::read_to_string(path)?.replace("\r\n", "\n") == bytes,
            "Focuser configuration contract is stale; regenerate it with export_focuser"
        );
    } else {
        std::fs::write(path, bytes)?;
    }
    Ok(())
}
