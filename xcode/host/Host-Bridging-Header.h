//  Host-Bridging-Header.h
//  Bridges the UniFFI-generated C ABI into the host app's Swift.
//
//  The app is a SwiftUI menu-bar app whose logic lives in the Rust
//  `fskit-s3-app` staticlib, reached through a UniFFI contract (see
//  app/src/ffi.rs). `uniffi-bindgen` emits two things into Generated/: the Swift
//  API (fskit_s3_app.swift, compiled as a target source) and this low-level C
//  header. The generated Swift does `#if canImport(fskit_s3_appFFI)` and, when
//  that module isn't provided separately, falls back to the C symbols being
//  visible in-module — which this bridging header makes so. The symbols
//  themselves are provided by the linked libfskit_s3_app.a.
#import "Generated/fskit_s3_appFFI.h"
