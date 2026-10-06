//! Generate the contract fixture consumed by the web and .NET setup readers.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("Usage: export_config PATH [--check]")?;
    let check = args.next();
    if check.as_deref().is_some_and(|s| s != "--check") || args.next().is_some() {
        return Err("Unknown option".into());
    }
    let value = regain_hub::description::describe_config(&[]);
    let bytes = serde_json::to_string_pretty(&value)? + "\n";
    if check.is_some() {
        if std::fs::read_to_string(&path)?.replace("\r\n", "\n") != bytes {
            return Err(
                "Hub configuration contract is stale; regenerate it with export_config".into(),
            );
        }
    } else {
        std::fs::write(path, bytes)?;
    }
    Ok(())
}
