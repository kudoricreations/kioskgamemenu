fn main() {
    // Puts src/kms_format.rs's ioctl in the dynamic symbol table, where shared
    // libraries find it first. Executables export nothing by default.
    println!("cargo:rustc-link-arg-bins=-Wl,--export-dynamic-symbol=ioctl");
    println!("cargo:rerun-if-changed=build.rs");
}
