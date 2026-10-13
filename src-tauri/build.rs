fn main() {
    println!("cargo:rerun-if-env-changed=APERTURE_NATIVE_DIR");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let native = std::env::var_os("APERTURE_NATIVE_DIR")
            .expect("APERTURE_NATIVE_DIR must name the pinned libvips SDK");
        println!(
            "cargo:rustc-link-search=native={}",
            std::path::PathBuf::from(native).join("lib").display()
        );
    }
    tauri_build::build()
}
