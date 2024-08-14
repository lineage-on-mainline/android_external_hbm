// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use std::path::PathBuf;
use std::{env, fs, os::unix};

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let pkg_name = env::var("CARGO_PKG_NAME").unwrap().replace('-', "_");
    let out_dir = env::var("OUT_DIR").unwrap();

    let hdr_name = format!("{}.h", pkg_name);
    let out_path = PathBuf::from(out_dir).join(&hdr_name);

    let _changed = match cbindgen::generate(manifest_dir) {
        Ok(bindings) => bindings.write_to_file(&out_path),
        Err(cbindgen::Error::ParseSyntaxError { .. }) => false,
        Err(err) => panic!("{:?}", err),
    };

    // this is not guaranteed, but out_path should be
    // <workspace>/target/<profile>/build/<pkg_name-hash>/out/<hdr_name>
    let depth = out_path.components().count();
    if depth > 6 {
        let mut link_path = PathBuf::new();
        let mut link_target = PathBuf::new();
        for (idx, comp) in out_path.components().enumerate() {
            if idx + 4 < depth {
                link_path.push(comp);
            } else {
                link_target.push(comp);
            }
        }
        link_path.push(hdr_name);

        if link_path.is_symlink() && link_path.read_link().unwrap() != link_target {
            let _ = fs::remove_file(&link_path);
        }

        if !link_path.exists() {
            unix::fs::symlink(link_target, link_path).expect("failed to create symlink");
        }
    }

    println!("cargo:rerun-if-changed=src");
}
