use std::{env, fs, path::PathBuf};

fn main() {
    let guest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../../debug-channel/guest/src");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let module = out.join("engine_events");
    fs::create_dir_all(&module).unwrap();
    // Preserve Rust's nested-module lookup for the shared guest source.
    for (from, to) in [
        ("engine_events.rs", "mod.rs"),
        ("engine_events/fragments.rs", "fragments.rs"),
    ] {
        println!("cargo:rerun-if-changed={}", guest.join(from).display());
        fs::copy(guest.join(from), module.join(to)).unwrap();
    }
    fs::write(out.join("guest_modules.rs"), "mod engine_events;\n").unwrap();
}
