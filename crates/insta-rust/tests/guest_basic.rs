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

/// Builds a minimal valid 64-bit x86-64 ELF with one `PT_LOAD` segment for validation tests.
fn synthetic_elf(e_type: u16, entry: u64, load_addr: u64) -> Vec<u8> {
    let mut buf = vec![0u8; 0x200];
    // ELF64 little-endian header (64 bytes).
    buf[0..4].copy_from_slice(b"\x7fELF");
    buf[4] = 2; // ELFCLASS64
    buf[5] = 1; // ELFDATA2LSB
    buf[6] = 1; // EV_CURRENT
    buf[16..18].copy_from_slice(&e_type.to_le_bytes());
    buf[18..20].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64
    buf[20..24].copy_from_slice(&1u32.to_le_bytes()); // EV_CURRENT
    buf[24..32].copy_from_slice(&entry.to_le_bytes());
    buf[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
    buf[52..54].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
    buf[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
    buf[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum

    // Single PT_LOAD program header at offset 64 (56 bytes).
    let ph = &mut buf[64..120];
    ph[0..4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
    ph[4..8].copy_from_slice(&5u32.to_le_bytes()); // PF_R | PF_X
    ph[8..16].copy_from_slice(&0x100u64.to_le_bytes()); // p_offset
    ph[16..24].copy_from_slice(&load_addr.to_le_bytes()); // p_vaddr
    ph[24..32].copy_from_slice(&load_addr.to_le_bytes()); // p_paddr
    ph[32..40].copy_from_slice(&0x10u64.to_le_bytes()); // p_filesz
    ph[40..48].copy_from_slice(&0x10u64.to_le_bytes()); // p_memsz
    ph[48..56].copy_from_slice(&0x1000u64.to_le_bytes()); // p_align
    buf
}

#[test]
fn rejects_pie_elf() {
    let pie = synthetic_elf(3, 0x20_0000, 0x20_0000); // ET_DYN = 3
    assert!(matches!(
        ElfImage::from_bytes(pie),
        Err(Error::InvalidElf { .. })
    ));
}

#[test]
fn rejects_elf_overlapping_page_tables() {
    let elf = ElfImage::from_bytes(synthetic_elf(2, 0x2000, 0x2000)).expect("parse ELF");
    assert!(matches!(
        Instance::boot(&elf, Config::default()),
        Err(Error::InvalidElf { .. })
    ));
}

#[test]
fn rejects_entry_outside_load_segments() {
    let elf = ElfImage::from_bytes(synthetic_elf(2, 0x30_0000, 0x20_0000)).expect("parse ELF");
    assert!(matches!(
        Instance::boot(&elf, Config::default()),
        Err(Error::InvalidElf { .. })
    ));
}

#[test]
fn rejects_non_2mib_or_undersized_ram_before_allocating() {
    let elf = ElfImage::from_bytes(synthetic_elf(2, 0x20_0000, 0x20_0000)).expect("parse ELF");
    for bad in [4096, 4 * 1024 * 1024 + 4096, 2 * 1024 * 1024 * 1024] {
        assert!(matches!(
            Instance::boot(&elf, Config { ram_bytes: bad }),
            Err(Error::BadSize { size_bytes }) if size_bytes == bad
        ));
    }
}
