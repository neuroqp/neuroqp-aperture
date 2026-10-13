This is libvips-rust-bindings 2.3.0 (MIT), copied from the crates.io source.
The Rust wrappers and Linux bindings remain upstream's; Aperture adds:

- Windows bindings generated from the pinned native SDK with the MSVC x64 ABI.
  Linux-generated `guint64 = c_ulong` and GObject layouts are unsafe on Win64.
- A Rust File-backed VipsSourceCustom reader. This avoids the filename option
  parser, symlink privileges, copying source pixels, and sharing CRT descriptors
  across MSVC and MinGW-built DLLs.
- Target finalization calls the supported vips_target_end API.
- Suppression of upstream generated-code warnings for the path dependency.

Windows builds need LLVM/libclang and APERTURE_NATIVE_DIR pointing to the SDK
prepared by the Windows packaging scripts. Linux builds retain the shipped
bindings (with C runtime size signatures corrected to usize) and link to the system libvips, GLib and GObject libraries.
