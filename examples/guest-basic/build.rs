fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    println!("cargo:rustc-link-arg=-T{manifest_dir}/link.ld");
    println!("cargo:rustc-link-arg=-no-pie");
    println!("cargo:rerun-if-changed=link.ld");
}
