// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let pkg_name = env::var("CARGO_PKG_NAME").unwrap().replace('-', "_");
    let out_dir = env::var("OUT_DIR").unwrap();

    let hdr_name = format!("{}.h", pkg_name);
    let out_path = PathBuf::from(out_dir).join(hdr_name);

    match cbindgen::generate(manifest_dir) {
        Ok(bindings) => {
            bindings.write_to_file(out_path);
        }
        Err(cbindgen::Error::ParseSyntaxError { .. }) => {}
        Err(err) => panic!("{:?}", err),
    };

    println!("cargo:rerun-if-changed=src");
}
