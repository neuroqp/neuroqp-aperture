fn main() {
    println!("cargo:rerun-if-env-changed=APERTURE_NATIVE_DIR");
    #[cfg(windows)]
    windows_bindings();
    for library in ["vips", "glib-2.0", "gobject-2.0"] {
        println!("cargo:rustc-link-lib={library}");
    }
}

#[cfg(windows)]
fn windows_bindings() {
    let native = std::path::PathBuf::from(
        std::env::var_os("APERTURE_NATIVE_DIR")
            .expect("APERTURE_NATIVE_DIR must name the pinned libvips SDK"),
    );
    println!(
        "cargo:rustc-link-search=native={}",
        native.join("lib").display()
    );
    let bindings = bindgen::Builder::default()
        .header("vips.h")
        .clang_arg("--target=x86_64-pc-windows-msvc")
        .clang_arg(format!("-I{}", native.join("include").display()))
        .clang_arg(format!("-I{}", native.join("include/glib-2.0").display()))
        .clang_arg(format!(
            "-I{}",
            native.join("lib/glib-2.0/include").display()
        ))
        // Match the existing wrapper's u64 sizes on our Windows x64 target.
        .size_t_is_usize(false)
        .generate_comments(false)
        .layout_tests(false)
        .derive_default(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("Generate Windows bindings from the pinned SDK, never Linux layouts");
    bindings
        .write_to_file(
            std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("bindings.rs"),
        )
        .unwrap();
}
