//! Binary entry point: hand-rolled CLI around the GTK application.

use std::process::ExitCode;

fn main() -> ExitCode {
    marksight::timing::init();
    for arg in std::env::args_os().skip(1) {
        let Some(flag) = arg.to_str() else {
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
            _ if flag.starts_with('-') => {
                eprintln!("marksight: unknown option {flag}");
                print_help();
                return ExitCode::FAILURE;
            }
            _ => {}
        }
    }
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
