//  Host-Bridging-Header.h
//  Exposes the Rust app's C entry point to the host's Swift bootstrap
//  (xcode/host/main.swift). Implemented by the linked fskit-s3-app staticlib
//  (see app/src/lib.rs: `#[no_mangle] pub extern "C" fn fskit_s3_app_run`).

void fskit_s3_app_run(void);
