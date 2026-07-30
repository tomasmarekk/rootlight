//! Process-boundary checks for CLI help and version discovery.
//!
//! Discovery must remain useful before any daemon or private state exists.

use std::process::Command;

fn run(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rootlight"))
        .args(arguments)
        .output()
        .expect("CLI process starts")
}

#[test]
fn no_arguments_and_help_return_bounded_human_discovery() {
    for arguments in [&[][..], &["--help"][..]] {
        let output = run(arguments);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).expect("help output is UTF-8");
        assert!(stdout.starts_with("Rootlight\n\nUsage:\n"));
        assert!(stdout.contains("repo index <root>"));
        assert!(stdout.contains("operation status <operation-id>"));
        assert!(stdout.len() < 4_096);
    }
}

#[test]
fn version_supports_human_and_stable_json_output() {
    let human = run(&["--version"]);
    assert!(human.status.success());
    assert!(human.stderr.is_empty());
    assert_eq!(
        String::from_utf8(human.stdout).expect("version output is UTF-8"),
        format!("rootlight {}\n", env!("CARGO_PKG_VERSION"))
    );

    let json = run(&["--version", "--json"]);
    assert!(json.status.success());
    assert!(json.stderr.is_empty());
    let envelope: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("version output is valid JSON");
    assert_eq!(envelope["contract_version"], "1.0");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["type"], "version");
    assert_eq!(envelope["result"]["data"]["name"], "rootlight");
    assert_eq!(
        envelope["result"]["data"]["version"],
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn help_supports_stable_json_output() {
    let output = run(&["--json", "--help"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("help output is valid JSON");
    assert_eq!(envelope["contract_version"], "1.0");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["type"], "help");
    assert_eq!(
        envelope["result"]["data"]["usage"],
        "rootlight <command> [options]"
    );
    assert!(
        envelope["result"]["data"]["repository_commands"]
            .as_array()
            .is_some_and(|commands| commands.len() == 5)
    );
}
