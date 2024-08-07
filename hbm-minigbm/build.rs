// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let pkg_name = env::var("CARGO_PKG_NAME").unwrap().replace('-', "_");
    let out_dir = env::var("OUT_DIR").unwrap();

    let copyright = "// Copyright 2024 Google LLC\n// SPDX-License-Identifier: MIT";
    let hdr_name = format!("{}.h", pkg_name);
    let hdr_guard = format!("{}_H", pkg_name.to_uppercase());
    let out_path = PathBuf::from(out_dir).join(hdr_name);

    cbindgen::Builder::new()
        .with_crate(manifest_dir)
        .with_header(copyright)
        .with_sys_include("sys/types.h")
        .with_include_guard(hdr_guard)
        .with_include_version(true)
        .with_language(cbindgen::Language::C)
        .with_cpp_compat(true)
        .generate()
        .expect("failed to generate bindings")
        .write_to_file(out_path);

    println!("cargo:rerun-if-changed=src");
}
