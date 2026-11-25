use std::path::{Path, PathBuf};
use std::process::Command;

use insta_rust::{Config, ElfImage, Error, ExitStatus, Instance};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/insta-rust has workspace root two levels up")
        .to_path_buf()
}

fn build_guest_basic() -> PathBuf {
    let root = workspace_root();
    let manifest = root.join("examples").join("guest-basic").join("Cargo.toml");
    let status = Command::new(env!("CARGO"))
        .args([
            "build",
            "--manifest-path",
            manifest.to_str().expect("UTF-8 manifest path"),
            "--target",
            "x86_64-unknown-none",
        ])
        .status()
        .expect("invoke cargo build for guest-basic");
    assert!(status.success(), "building examples/guest-basic failed");

    root.join("examples")
        .join("guest-basic")
        .join("target")
        .join("x86_64-unknown-none")
        .join("debug")
        .join("guest-basic")
}

#[test]
fn boots_and_runs_guest_basic_on_whp() {
    assert!(
        insta_rust::is_available(),
        "Windows Hypervisor Platform is not enabled on this host"
    );
    let elf_path = build_guest_basic();
    let elf = ElfImage::from_file(&elf_path).expect("load guest-basic ELF");
    let mut instance = Instance::boot(&elf, Config::default()).expect("boot guest-basic on WHP");
    let output = instance.run().expect("run guest-basic on WHP");

    assert_eq!(output.exit, ExitStatus::Halted);
    assert_eq!(
        output.stdout_str(),
        "hello from bare-metal rust! sum(1..=100)=5050 bss_prev=0\n"
    );
}

#[test]
fn rejects_invalid_elf_bytes() {
    assert!(matches!(
        ElfImage::from_bytes(b"not an elf".to_vec()),
        Err(Error::InvalidElf { .. })
    ));
}
