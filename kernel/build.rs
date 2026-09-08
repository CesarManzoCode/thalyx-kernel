fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    println!("cargo::rustc-link-arg-bins=-T{dir}/link.ld");
    println!("cargo::rustc-link-arg-bins=-zmax-page-size=4096");
    println!("cargo::rerun-if-changed={dir}/link.ld");
}
