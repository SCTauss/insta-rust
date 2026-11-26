//! Thin native Cargo `runner` hook for `insta-rust` bare-metal WHP guests.
#![forbid(unsafe_code)]

use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use insta_rust::{Config, ElfImage, ExitStatus, Instance};

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("insta-runner: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let mut args = env::args_os().skip(1);
    let elf_path = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| "usage: insta-runner <guest-elf> [guest-args...]".to_string())?;

    let guest_input = args
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");

    let timeout = match env::var("INSTA_TIMEOUT_MS") {
        Ok(raw) if !raw.trim().is_empty() => {
            let ms: u64 = raw
                .trim()
                .parse()
                .map_err(|err| format!("invalid INSTA_TIMEOUT_MS '{raw}': {err}"))?;
            Some(Duration::from_millis(ms))
        }
        _ => None,
    };

    let config = Config {
        timeout,
        ..Config::default()
    };
    let elf = ElfImage::from_file(&elf_path).map_err(|err| err.to_string())?;
    let mut instance = Instance::boot(&elf, config).map_err(|err| err.to_string())?;
    let output = instance
        .run_with_input(guest_input.as_bytes())
        .map_err(|err| err.to_string())?;

    let mut stdout = io::stdout().lock();
    stdout
        .write_all(&output.stdout)
        .and_then(|()| stdout.flush())
        .map_err(|err| format!("failed to write guest stdout: {err}"))?;

    match output.exit {
        ExitStatus::Halted => Ok(ExitCode::SUCCESS),
        ExitStatus::Faulted(report) => {
            eprintln!("{report}");
            Ok(ExitCode::FAILURE)
        }
        ExitStatus::TimedOut {
            elapsed,
            regs,
            location,
        } => {
            eprintln!("guest timed out after {}ms", elapsed.as_millis());
            if let Some(loc) = location {
                eprintln!("  at {loc}");
            }
            eprintln!("  rip={:#x} rsp={:#x}", regs.rip, regs.rsp);
            Ok(ExitCode::FAILURE)
        }
        ExitStatus::Unhandled { code } => Err(format!("guest exited with unhandled code {code}")),
    }
}
