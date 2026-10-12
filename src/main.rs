//! Binary entry point: hand-rolled CLI around the GTK application.

use std::process::{Command, ExitCode};

/// Hidden flag: this process is a throwaway forwarder started by a previous
/// launch to hand its files to a running instance (see `marksight::app::
/// forward_files`). Never advertised; users have no reason to type it.
const FORWARD_PROBE: &str = "--__forward-probe";

fn main() -> ExitCode {
    marksight::timing::init();
    let mut probe = false;
    let mut files: Vec<std::ffi::OsString> = Vec::new();
    for arg in std::env::args_os().skip(1) {
        let Some(flag) = arg.to_str() else {
            files.push(arg);
            continue;
        };
        match flag {
            "-h" | "--help" => {
                print_help();
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("marksight {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            FORWARD_PROBE => probe = true,
            _ if flag.starts_with('-') => {
                eprintln!("marksight: unknown option {flag}");
                print_help();
                return ExitCode::FAILURE;
            }
            _ => files.push(arg),
        }
    }
    if probe {
        return if marksight::app::forward_files(&files) {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(3)
        };
    }
    // Hand the files to a running instance, if any, through a child process
    // and quit: the child talks to the session bus and exits in milliseconds,
    // so this process never initializes GTK or waits for the primary.
    // GTK's constructor strips DESKTOP_STARTUP_ID from the environment
    // before main runs, so the launch's startup id is recovered from the
    // exec-time environment and handed to the child explicitly; without it
    // the running instance could never end the desktop's launch feedback.
    if let Ok(exe) = std::env::current_exe() {
        let mut forward = Command::new(exe);
        forward.arg(FORWARD_PROBE).args(&files);
        if let Some(id) = marksight::app::launch_startup_id() {
            forward.env("DESKTOP_STARTUP_ID", id);
        }
        if let Ok(status) = forward.status() {
            if status.code() == Some(0) {
                return ExitCode::SUCCESS;
            }
        }
    }
    // Pick up the user's locale and select the message domain before any
    // translatable string is shown. The forwarder and CLI paths print
    // untranslated diagnostics by design.
    marksight::i18n::init();
    marksight::app::run().into()
}

fn print_help() {
    println!(
        "marksight {} — a minimal, theme-native markdown viewer

Usage: marksight [FILE]

Options:
  -h, --help     Print this help
  -V, --version  Print version

Keyboard:
  Ctrl+O  open a file        F9      toggle contents sidebar
  Ctrl+F  find in document   Esc     close find
  Ctrl+W  close window       Ctrl+Q  quit",
        env!("CARGO_PKG_VERSION")
    );
}
