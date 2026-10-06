//! `junk sony replay` end to end: the built binary over
//! `fixtures/sony-wh1000xm4/synthetic-init-status.trace`.
//!
//! That trace is synthetic (see `junk-sony`'s `tests/recorded.rs`): these tests prove the
//! CLI's plumbing and report, not what a real headset says.

use std::process::{Command, Output};

const TRACE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/sony-wh1000xm4/synthetic-init-status.trace"
);

/// Runs `junk` with `args`.
fn junk(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_junk"))
        .args(args)
        .output()
        .expect("the binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn the_text_report_has_every_group_and_no_raw_replies() {
    let output = junk(&["sony", "replay", TRACE]);
    let out = stdout(&output);
    assert!(output.status.success(), "{out}\n{}", stderr(&output));
    for line in [
        "model                   WH-1000XM4",
        "firmware                1.2.3",
        "protocol version        0x4010",
        "unique id               ABCD (capability counter 7)",
        "battery                 80% NotCharging",
        "battery left/right      not supported",
        "codec                   Ldac",
        "ambient Normal level 5 of 10",
        "equalizer range         6 bands, 21 levels (0 to 20)",
        "clear bass              10",
        "general setting         TouchPanelSetting -> On",
        "serial                  ABCDE",
        "Paired devices",
        "capability              up to 8 paired, 2 connected",
        "pairing mode            Normal (Enable)",
        "device                  Phone  AA:BB:CC:DD:EE:01  connected (1)  <- playback",
        "device                  Laptop  AA:BB:CC:DD:EE:02  not connected\n",
    ] {
        assert!(out.contains(line), "missing {line:?} in\n{out}");
    }
    assert!(!out.contains("Raw replies"), "{out}");
    let err = stderr(&output);
    assert!(err.contains("protocol version 0x4010 (supported)"), "{err}");
}

#[test]
fn raw_adds_the_init_replies_as_hex_lines() {
    let output = junk(&["sony", "replay", TRACE, "--raw"]);
    let out = stdout(&output);
    assert!(output.status.success(), "{out}\n{}", stderr(&output));
    assert!(out.contains("Raw replies"), "{out}");
    assert!(out.contains("  0c  01004010\n"), "{out}");
    assert!(out.contains("  0e  47010501\n"), "{out}");
}

#[test]
fn json_is_one_document_with_the_same_values() {
    let output = junk(&["sony", "replay", TRACE, "--json"]);
    let out = stdout(&output);
    assert!(output.status.success(), "{out}\n{}", stderr(&output));
    let json: serde_json::Value = serde_json::from_str(&out).expect("one JSON document");
    assert_eq!(json["device"]["model"]["Value"], "WH-1000XM4");
    assert_eq!(json["device"]["protocol_version"]["Value"], 0x4010);
    assert_eq!(json["battery"]["Value"]["level"], 80);
    assert_eq!(json["codec"]["Value"], "Ldac");
    assert_eq!(json["battery_cradle"], "NotSupported");
    assert_eq!(json["serial"]["Value"], "ABCDE");
    assert_eq!(json["capabilities"]["pairing"]["Value"]["max_paired"], 8);
    assert_eq!(json["capabilities"]["pairing"]["Value"]["max_connected"], 2);
    assert_eq!(json["pairing_mode"]["Value"]["mode"], "Normal");
    let devices = &json["paired_devices"]["Value"];
    assert_eq!(devices["devices"][0]["name"], "Phone");
    assert_eq!(devices["devices"][0]["connection"]["Connected"], 1);
    assert_eq!(devices["devices"][1]["connection"], "NotConnected");
    assert_eq!(devices["playback"]["Order"], 1);
    assert_eq!(
        json["raw_replies"][0]["payload"],
        serde_json::json!([1, 0, 0x40, 0x10])
    );
}

#[test]
fn a_missing_file_fails_with_one_line() {
    let output = junk(&["sony", "replay", "/nonexistent/none.trace"]);
    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    let err = stderr(&output);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(
        err.starts_with("junk: cannot read /nonexistent/none.trace"),
        "{err}"
    );
}

#[test]
fn a_file_that_is_not_a_trace_fails_with_one_line() {
    let path = std::env::temp_dir().join(format!("junk-cli-sony-{}.txt", std::process::id()));
    std::fs::write(&path, "this is not a trace\n").expect("a temp file");
    let output = junk(&["sony", "replay", &path.display().to_string()]);
    let _ = std::fs::remove_file(&path);
    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    let err = stderr(&output);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.starts_with("junk: "), "{err}");
}

#[test]
fn a_trace_of_another_device_is_refused() {
    let colmi = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/colmi-r10/thering-realtime-2025-11-19.trace"
    );
    let output = junk(&["sony", "replay", colmi]);
    assert!(!output.status.success());
    assert_eq!(stderr(&output).lines().count(), 1, "{}", stderr(&output));
}
