//! Thin native Cargo `runner` hook for `insta-rust` bare-metal WHP guests.
#![forbid(unsafe_code)]

use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

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
    let elf_path = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| "usage: insta-runner <guest-elf>".to_string())?;

    let elf = ElfImage::from_file(&elf_path).map_err(|err| err.to_string())?;
    let mut instance = Instance::boot(&elf, Config::default()).map_err(|err| err.to_string())?;
    let output = instance.run().map_err(|err| err.to_string())?;

    let mut stdout = io::stdout().lock();
    stdout
        .write_all(&output.stdout)
        .and_then(|()| stdout.flush())
        .map_err(|err| format!("failed to write guest stdout: {err}"))?;

    match output.exit {
        ExitStatus::Halted => Ok(ExitCode::SUCCESS),
        ExitStatus::Unhandled { code } => Err(format!("guest exited with unhandled code {code}")),
    }
}
