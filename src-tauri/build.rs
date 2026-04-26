fn main() {
    // On macOS, embed an LC_RPATH entry pointing at the .app's
    // Frameworks/ directory. Hardened-runtime apps disable the
    // dyld fallback library paths, so without an explicit rpath
    // libndi.dylib (install_name @rpath/libndi.dylib) wouldn't
    // resolve at launch — the .app would crash with
    // "Library not loaded: @rpath/libndi.dylib" the moment the
    // streamer reaches into ndi_runtime::init().
    //
    // The matching copy of libndi.dylib into Contents/Frameworks/
    // is driven by `bundle.macOS.frameworks` in tauri.conf.json.
    //
    // In `cargo tauri dev` this rpath is harmless — the dyld
    // search falls through to the system /usr/local/lib/ where
    // NDI Tools installs the dylib. Production .app bundles
    // ship libndi inside Frameworks/ and resolve via this rpath.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
    }
    tauri_build::build()
}
