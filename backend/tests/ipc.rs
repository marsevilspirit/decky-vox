use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[test]
fn handshake_snapshot_malformed_and_shutdown_are_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let plugin = root.path().join("plugin");
    let settings = root.path().join("settings");
    let runtime = root.path().join("runtime");
    let logs = root.path().join("logs");
    for directory in [&plugin, &settings, &runtime, &logs] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::write(
        settings.join("settings.json"),
        serde_json::to_vec(&json!({"auto_start": false})).unwrap(),
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_decky-vox-core"))
        .args([
            "--plugin-dir",
            plugin.to_str().unwrap(),
            "--settings-dir",
            settings.to_str().unwrap(),
            "--runtime-dir",
            runtime.to_str().unwrap(),
            "--log-dir",
            logs.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    writeln!(
        stdin,
        "{}",
        json!({"v":1,"kind":"request","id":0,"method":"hello","params":{}})
    )
    .unwrap();
    let missing_version = read_value(&mut stdout);
    assert_eq!(missing_version["ok"], false);
    assert_eq!(missing_version["error"]["code"], "PROTOCOL_MISMATCH");

    writeln!(
        stdin,
        "{}",
        json!({"v":1,"kind":"request","id":10,"method":"hello","params":{"protocol_version":"1"}})
    )
    .unwrap();
    let string_version = read_value(&mut stdout);
    assert_eq!(string_version["ok"], false);
    assert_eq!(string_version["error"]["code"], "PROTOCOL_MISMATCH");

    writeln!(
        stdin,
        "{}",
        json!({"v":1,"kind":"request","id":1,"method":"hello","params":{"protocol_version":1}})
    )
    .unwrap();
    let hello = read_value(&mut stdout);
    assert_eq!(object_keys(&hello), vec!["id", "kind", "ok", "result", "v"]);
    assert_eq!(hello["v"], 1);
    assert_eq!(hello["kind"], "response");
    assert_eq!(hello["id"], 1);
    assert_eq!(hello["ok"], true);
    assert_eq!(hello["result"]["protocol_version"], 1);
    assert!(hello["result"]["instance_id"].is_string());

    writeln!(
        stdin,
        "{}",
        json!({"v":1,"kind":"request","id":2,"method":"get_snapshot","params":{}})
    )
    .unwrap();
    let snapshot = read_value(&mut stdout);
    assert_eq!(
        object_keys(&snapshot["result"]),
        vec![
            "enabled",
            "engine_backend",
            "error",
            "instance_id",
            "model_installed",
            "phase",
            "protocol_version",
            "seq",
            "settings",
        ]
    );
    assert_eq!(snapshot["result"]["phase"], "stopped");
    assert_eq!(snapshot["result"]["settings"]["model"], "small");

    writeln!(
        stdin,
        "{}",
        json!({"v":1,"kind":"request","id":20,"method":"not_a_method","params":{}})
    )
    .unwrap();
    let unknown = read_value(&mut stdout);
    assert_eq!(unknown["ok"], false);
    assert_eq!(unknown["error"]["code"], "UNKNOWN_METHOD");
    let error_event = read_value(&mut stdout);
    let snapshot_event = read_value(&mut stdout);
    assert_eq!(error_event["kind"], "event");
    assert_eq!(error_event["name"], "error");
    assert_eq!(snapshot_event["name"], "snapshot");
    assert_eq!(snapshot_event["payload"]["seq"], snapshot_event["seq"]);

    writeln!(
        stdin,
        "{}",
        json!({"v":2,"kind":"request","id":21,"method":"get_snapshot","params":{}})
    )
    .unwrap();
    let version = read_value(&mut stdout);
    assert_eq!(version["ok"], false);
    assert_eq!(version["id"], 21);
    assert_eq!(version["error"]["code"], "PROTOCOL_MISMATCH");

    writeln!(stdin, "{{").unwrap();
    let malformed = read_value(&mut stdout);
    assert_eq!(malformed["ok"], false);
    assert_eq!(malformed["error"]["code"], "MALFORMED_REQUEST");
    assert!(malformed["id"].is_null());

    writeln!(
        stdin,
        "{}",
        json!({"v":1,"kind":"request","id":3,"method":"shutdown","params":{}})
    )
    .unwrap();
    let shutdown = read_value(&mut stdout);
    assert_eq!(shutdown["ok"], true);
    assert_eq!(shutdown["result"]["shutting_down"], true);
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

fn read_value(reader: &mut impl BufRead) -> Value {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn object_keys(value: &Value) -> Vec<&str> {
    let mut keys = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    keys
}
