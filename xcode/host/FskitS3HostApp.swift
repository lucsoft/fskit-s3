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
//
//  It also flags a STALE registration: FSKit reports the on-disk URL of the
//  extension it will actually launch (`FSModuleIdentity.url`). We compare that
//  bundle's path + CFBundleVersion against the .appex embedded in THIS running
//  host app. If they differ, fskitd will launch a different build than the one
//  you just made — the usual cause of "I rebuilt but nothing changed." mtimes
//  can't tell you this (git rewrites them on checkout); a path/version compare of
//  the actual bundles can.

import AppKit
import FSKit
import SwiftUI

private let extensionBundleID = "dev.lucsoft.fskit-s3.ext"

@main
struct FskitS3HostApp: App {
    var body: some Scene {
        WindowGroup("fskit-s3") {
            HealthView()
                .frame(width: 460, height: 360)
        }
        .windowResizability(.contentSize)
    }
}

/// Result of asking FSKit whether our module is installed + enabled.
private enum Health: Equatable {
    case checking
    case notInstalled
    case disabled
    case ready
    case error(String)
}

/// Identity of an extension bundle on disk: its build version and location.
private struct BuildInfo: Equatable {
    let version: String  // CFBundleVersion
    let path: String     // resolved bundle path
}

/// How the registered (will-launch) extension compares to the one this app ships.
private enum Freshness: Equatable {
    case unknown
    case fresh(BuildInfo)                                    // registered == embedded
    case stale(registered: BuildInfo, embedded: BuildInfo?)  // a different build will run
}

private struct HealthView: View {
    @State private var health: Health = .checking
    @State private var freshness: Freshness = .unknown
    // Poll so the status "heals" itself when the extension is enabled in Settings.
    private let ticker = Timer.publish(every: 2, on: .main, in: .common).autoconnect()

    var body: some View {
        VStack(spacing: 18) {
            Text("fskit-s3").font(.largeTitle.bold())
            statusRow
            freshnessRow
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

    @ViewBuilder private var freshnessRow: some View {
        switch freshness {
        case .unknown:
            EmptyView()
        case .fresh(let info):
            status(.green, "checkmark.seal.fill",
                   "Registered build matches this app (v\(info.version)).")
        case .stale(let registered, let embedded):
            let mine = embedded.map { "this app carries v\($0.version)" } ?? "this app has no embedded extension"
            status(.orange, "exclamationmark.triangle.fill",
                   """
                   Stale: fskitd will launch a DIFFERENT build than this app.
                   Registered: v\(registered.version) — \(registered.path)
                   (\(mine).) Re-run this app, or reset with: sudo killall fskitd
                   """)
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
                freshness = .unknown
                return
            }
            health = mine.isEnabled ? .ready : .disabled
            freshness = compareBuilds(registeredURL: mine.url)
        } catch {
            health = .error(error.localizedDescription)
            freshness = .unknown
        }
    }

    /// Compare the extension FSKit will launch (`registeredURL`) with the .appex
    /// embedded in this running host app.
    private func compareBuilds(registeredURL: URL) -> Freshness {
        let embedded = embeddedExtensionInfo()
        guard let registered = buildInfo(at: registeredURL) else {
            // Can't read the registered bundle — treat unknown rather than alarm.
            return .unknown
        }
        if let embedded, embedded == registered {
            return .fresh(registered)
        }
        return .stale(registered: registered, embedded: embedded)
    }

    /// The extension bundle shipped inside this host app (`Contents/Extensions`).
    private func embeddedExtensionInfo() -> BuildInfo? {
        let extDir = Bundle.main.bundleURL.appendingPathComponent("Contents/Extensions")
        let entries = (try? FileManager.default.contentsOfDirectory(
            at: extDir, includingPropertiesForKeys: nil)) ?? []
        let match = entries.first { Bundle(url: $0)?.bundleIdentifier == extensionBundleID }
        return match.flatMap(buildInfo(at:))
    }

    /// Read `{version, resolved-path}` from a bundle on disk.
    private func buildInfo(at url: URL) -> BuildInfo? {
        guard let bundle = Bundle(url: url),
              let version = bundle.infoDictionary?["CFBundleVersion"] as? String
        else { return nil }
        let path = url.resolvingSymlinksInPath().standardizedFileURL.path
        return BuildInfo(version: version, path: path)
    }

    private func openExtensionSettings() {
        // System Settings ▸ General ▸ Login Items & Extensions ▸ File System Extensions.
        if let url = URL(string: "x-apple.systempreferences:com.apple.ExtensionsPreferences") {
            NSWorkspace.shared.open(url)
        }
    }
}
