//  FskitS3HostApp.swift
//  Minimal host app. Its only job is to carry the FSKit extension (macOS
//  requires an app to vend an extension). All file-system logic is in the Rust
//  `ext` staticlib behind the extension's Swift @main bootstrap.
//
//  The window is a live health check: it asks FSKit (FSClient) whether our module
//  is installed + enabled and self-refreshes, so enabling it in System Settings
//  flips the status to ✓ without reopening. It can't flip that toggle itself —
//  macOS gates enabling a file-system extension behind user consent — so it
//  deep-links to the right Settings pane instead.

import AppKit
import FSKit
import SwiftUI

private let extensionBundleID = "dev.lucsoft.fskit-s3.ext"

@main
struct FskitS3HostApp: App {
    var body: some Scene {
        WindowGroup("fskit-s3") {
            HealthView()
                .frame(width: 460, height: 300)
        }
        .windowResizability(.contentSize)
    }
}

/// Result of asking FSKit about our module.
private enum Health: Equatable {
    case checking
    case notInstalled
    case disabled
    case ready
    case error(String)
}

private struct HealthView: View {
    @State private var health: Health = .checking
    // Poll so the status "heals" itself when the extension is enabled in Settings.
    private let ticker = Timer.publish(every: 2, on: .main, in: .common).autoconnect()

    var body: some View {
        VStack(spacing: 18) {
            Text("fskit-s3").font(.largeTitle.bold())
            statusRow
            actions
            Text("You can close this window once it's ready — the extension runs on its own.")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .padding(28)
        .task { await refresh() }
        .onReceive(ticker) { _ in Task { await refresh() } }
    }

    @ViewBuilder private var statusRow: some View {
        switch health {
        case .checking:
            Label { Text("Checking…") } icon: { ProgressView().controlSize(.small) }
        case .notInstalled:
            status(.orange, "exclamationmark.triangle.fill",
                   "Extension not registered yet — give it a moment, or Build & Run again.")
        case .disabled:
            status(.orange, "exclamationmark.triangle.fill",
                   "Extension installed but not enabled.")
        case .ready:
            status(.green, "checkmark.circle.fill", "Extension enabled and ready.")
        case .error(let message):
            status(.red, "xmark.octagon.fill", message)
        }
    }

    @ViewBuilder private var actions: some View {
        HStack {
            if health == .disabled || health == .notInstalled {
                Button("Open System Settings…") { openExtensionSettings() }
                    .keyboardShortcut(.defaultAction)
            }
            Button("Re-check") { Task { await refresh() } }
        }
    }

    private func status(_ color: Color, _ symbol: String, _ text: String) -> some View {
        Label {
            Text(text).multilineTextAlignment(.center)
        } icon: {
            Image(systemName: symbol).foregroundStyle(color)
        }
        .labelStyle(.titleAndIcon)
    }

    @MainActor
    private func refresh() async {
        do {
            let modules = try await FSClient.shared.installedExtensions
            guard let mine = modules.first(where: { $0.bundleIdentifier == extensionBundleID })
            else {
                health = .notInstalled
                return
            }
            health = mine.isEnabled ? .ready : .disabled
        } catch {
            health = .error(error.localizedDescription)
        }
    }

    private func openExtensionSettings() {
        // System Settings ▸ General ▸ Login Items & Extensions ▸ File System Extensions.
        if let url = URL(string: "x-apple.systempreferences:com.apple.ExtensionsPreferences") {
            NSWorkspace.shared.open(url)
        }
    }
}
