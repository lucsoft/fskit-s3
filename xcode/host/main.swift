//  main.swift
//  The host app's entire bootstrap: hand control to the Rust app.
//
//  macOS requires an app to vend a file-system extension, so this target is the
//  app that carries the embedded FSKit `.appex`. But all UI + logic — the
//  status-bar menu, mounts, and the extension-health window that used to be this
//  app's SwiftUI window — now live in the Rust `fskit-s3-app` library, linked here
//  as a staticlib. This file just calls its C entry point, which runs the
//  AppKit event loop and never returns until the app quits (mirroring how the
//  extension target hands off to `fskit_s3_make_filesystem`).
//
//  `fskit_s3_app_run` is declared in Host-Bridging-Header.h.

import AppKit

fskit_s3_app_run()
