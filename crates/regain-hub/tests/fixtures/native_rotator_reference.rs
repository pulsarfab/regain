// Inert standalone worker fixture: no vendor crates, SDK, USB or serial calls.
use std::{fs, io::{self, BufRead, Write}, path::PathBuf};
fn number(line: &str, key: &str) -> f64 {
    let rest = line.split(&format!("\"{key}\":")).nth(1).unwrap();
    rest.split([',', '}']).next().unwrap().parse().unwrap()
}
fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    assert_eq!(&args[1..4], ["zwo", "caa", "serve"]);
    assert!(args.iter().any(|arg| arg == "--simulate"));
    assert!(args.iter().any(|arg| arg == "PRIVATE-REFERENCE"));
    let root = PathBuf::from(&args[0]).parent().unwrap().to_owned();
    let mut offset = 0.0;
    let mut output = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        let mut trace = fs::OpenOptions::new().create(true).append(true).open(root.join("commands.log")).unwrap();
        writeln!(trace, "{line}").unwrap(); trace.sync_all().unwrap();
        let result = if line.contains("\"command\":\"identity\"") {
            "{\"serial\":\"PRIVATE-REFERENCE\",\"simulation\":true}".to_owned()
        } else if line.contains("\"command\":\"status\"") {
            let logical = (152.0_f64 + offset).rem_euclid(360.0);
            format!("{{\"mechanical_degrees\":152.0,\"logical_degrees\":{logical},\"target_degrees\":{logical},\"logical_offset\":{offset},\"moving\":false,\"error\":0,\"temperature_c\":20.0}}")
        } else if line.contains("\"command\":\"settings\"") {
            "{\"reverse\":false}".to_owned()
        } else if line.contains("\"command\":\"sync\"") {
            let mode = fs::read_to_string(root.join("mode")).unwrap();
            if mode != "ignored" { offset = number(&line, "degrees") - 152.0; }
            fs::write(root.join("applied-offset"), offset.to_string()).unwrap();
            if mode == "lost" {
                std::process::exit(19);
            }
            "{\"accepted\":true}".to_owned()
        } else if line.contains("\"command\":\"restore-reference\"") {
            offset = number(&line, "offset");
            "{\"accepted\":true}".to_owned()
        } else { panic!("Unexpected worker command {line}"); };
        writeln!(output, "{{\"ok\":true,\"result\":{result}}}").unwrap();
        output.flush().unwrap();
    }
}
