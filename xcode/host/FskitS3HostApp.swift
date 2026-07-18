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

/// Identity of an extension bundle on disk: its build SHA, version, and location.
private struct BuildInfo: Equatable {
    let sha: String      // FSKitS3GitSHA (git describe --always --dirty)
    let version: String  // CFBundleVersion
    let path: String     // resolved bundle path

    /// A `-dirty` build was made from an uncommitted tree — two such builds can
    /// share a SHA yet differ, so a match on a dirty SHA isn't conclusive.
    var isDirty: Bool { sha.hasSuffix("-dirty") }
    /// The SHA is a usable identity (present and not the `unknown` placeholder).
    var hasSHA: Bool { !sha.isEmpty && sha != "unknown" }
}

/// How the registered (will-launch) extension compares to the one this app ships.
private enum Freshness: Equatable {
    case unknown
    case match(BuildInfo)                                    // same git SHA as this host
    case mismatch(registered: BuildInfo, host: BuildInfo?)   // a different build will run
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
        case .match(let info) where info.isDirty:
            status(.yellow, "checkmark.seal",
                   "Registered extension matches this app (build \(info.sha)) — dirty build, so equal SHAs aren't a guarantee.")
        case .match(let info):
            status(.green, "checkmark.seal.fill",
                   "Registered extension matches this app (build \(info.sha)).")
        case .mismatch(let registered, let host):
            let mine = host?.sha ?? "none"
            status(.orange, "exclamationmark.triangle.fill",
                   """
                   Build mismatch — fskitd will launch a DIFFERENT build than this app.
                   Registered extension: build \(registered.sha)
                   This app: build \(mine)
                   Re-run this app to re-register; if it persists: sudo killall fskitd
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

    /// Compare the git SHA of the extension FSKit will launch (`registeredURL`)
    /// against this host app's own SHA. The host and its embedded extension are
    /// built together, so their SHAs match by construction — a divergence means a
    /// different extension build (another branch, another copy, or a stale
    /// registration) is what will actually run.
    private func compareBuilds(registeredURL: URL) -> Freshness {
        guard let registered = buildInfo(at: registeredURL) else { return .unknown }
        // Need a real SHA on both sides to compare identities; otherwise stay
        // quiet rather than raise a false alarm (e.g. a pre-SHA build).
        guard let host = buildInfo(at: Bundle.main.bundleURL),
              registered.hasSHA, host.hasSHA
        else { return .unknown }
        return registered.sha == host.sha ? .match(registered)
                                          : .mismatch(registered: registered, host: host)
    }

    /// Read `{sha, version, resolved-path}` from a bundle on disk.
    private func buildInfo(at url: URL) -> BuildInfo? {
        guard let info = Bundle(url: url)?.infoDictionary else { return nil }
        let sha = info["FSKitS3GitSHA"] as? String ?? "unknown"
        let version = info["CFBundleVersion"] as? String ?? "?"
        let path = url.resolvingSymlinksInPath().standardizedFileURL.path
        return BuildInfo(sha: sha, version: version, path: path)
    }

    private func openExtensionSettings() {
        // System Settings ▸ General ▸ Login Items & Extensions ▸ File System Extensions.
        if let url = URL(string: "x-apple.systempreferences:com.apple.ExtensionsPreferences") {
            NSWorkspace.shared.open(url)
        }
    }
}
