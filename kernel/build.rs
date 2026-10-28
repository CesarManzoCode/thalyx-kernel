fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    println!("cargo::rustc-link-arg-bins=-T{dir}/link.ld");
    println!("cargo::rustc-link-arg-bins=-zmax-page-size=4096");
    // Identical functions and read-only constants are kept once. Nothing in the
    // kernel compares function or constant addresses for identity, so folding
    // shrinks the loaded image without changing an instruction that runs.
    println!("cargo::rustc-link-arg-bins=--icf=all");
    println!("cargo::rerun-if-changed={dir}/link.ld");
}
