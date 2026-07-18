//  FskitS3HostApp.swift
//  Minimal host app. Its only job is to carry the FSKit extension (macOS
//  requires an app to vend an extension). All file-system logic is in the
//  Rust `ext` staticlib behind the extension's Swift @main bootstrap.

import SwiftUI

@main
struct FskitS3HostApp: App {
    var body: some Scene {
        WindowGroup("fskit-s3") {
            VStack(spacing: 12) {
                Text("fskit-s3").font(.largeTitle.bold())
                Text("The FSKit file-system extension is bundled inside this app.")
                Text("Enable it in System Settings ▸ General ▸ Login Items & Extensions ▸ File System Extensions, then mount with `mount -F -t fskit-s3 …`.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            .padding(32)
            .frame(width: 460, height: 240)
        }
    }
}
