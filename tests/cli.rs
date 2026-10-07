//! CLI surface tests: run the real binary as a subprocess.

use std::process::Command;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_markview")
}

#[test]
fn version_prints_and_exits_clean() {
    let out = Command::new(binary()).arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("markview {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_prints_usage() {
    let out = Command::new(binary()).arg("--help").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("Usage: markview [FILE]"),
        "help text missing usage: {text}"
    );
    assert!(text.contains("--version"));
}

#[test]
fn unknown_flag_fails_with_hint() {
    let out = Command::new(binary()).arg("--nonsense").output().unwrap();
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(
        text.contains("unknown option"),
        "stderr should mention the bad flag: {text}"
    );
}
