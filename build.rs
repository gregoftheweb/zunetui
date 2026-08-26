use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");

    let libmtp = pkg_config::Config::new()
        .probe("libmtp")
        .expect("libmtp must be discoverable through pkg-config");

    let mut builder = bindgen::Builder::default()
        .header("wrapper.h")
        .allowlist_function("LIBMTP_.*")
        .allowlist_type("LIBMTP_.*")
        .allowlist_var("LIBMTP_.*")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    for include_path in libmtp.include_paths {
        builder = builder.clang_arg(format!("-I{}", include_path.display()));
    }

    let bindings = builder
        .generate()
        .expect("bindgen failed to generate libmtp bindings");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    bindings
        .write_to_file(out_dir.join("libmtp_bindings.rs"))
        .expect("failed to write libmtp bindings");
}
