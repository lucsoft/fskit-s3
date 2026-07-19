//  Health.swift
//  The extension-health window + the presentation mapping that turns a Rust
//  `Report` into SF Symbols, colours, and text.
//
//  This is the "presentation stays on the Swift side" half of the contract: the
//  Rust side (app/src/health.rs) decides *what state* the extension is in; this
//  file decides how to draw it. It mirrors the old Rust `healthwindow.rs`
//  (menu_glyphs / health_line / freshness_line) one-to-one.

import SwiftUI
import AppKit

/// Window identity for the health panel, shared by the `Window` scene and the
/// menu's `openWindow(id:)`.
enum HealthWindow {
    static let id = "health"
}

// MARK: - Menu-bar + row presentation

/// The menu-bar glyph and the menu's health-row glyph + label for a report.
struct HealthPresentation {
    var barSymbol: String
    var rowSymbol: String
    var rowColor: Color
    var rowText: String
}

/// Map a health [`Report`] to its menu-bar/row presentation. A build mismatch is
/// surfaced even when the extension is otherwise "ready".
func presentation(for report: Report) -> HealthPresentation {
    if case .mismatch = report.freshness {
        return HealthPresentation(
            barSymbol: "cloud.bolt",
            rowSymbol: "exclamationmark.triangle.fill",
            rowColor: .orange,
            rowText: "Extension ready — but a different build will run"
        )
    }
    switch report.health {
    case .ready:
        return HealthPresentation(
            barSymbol: "cloud.fill", rowSymbol: "checkmark.seal.fill",
            rowColor: .primary, rowText: "Extension ready")
    case .disabled:
        return HealthPresentation(
            barSymbol: "cloud.bolt", rowSymbol: "exclamationmark.triangle.fill",
            rowColor: .orange, rowText: "Extension not enabled — click to fix")
    case .notInstalled:
        return HealthPresentation(
            barSymbol: "cloud.bolt", rowSymbol: "exclamationmark.triangle.fill",
            rowColor: .orange, rowText: "Extension not registered — click to fix")
    case .error:
        return HealthPresentation(
            barSymbol: "cloud.bolt", rowSymbol: "xmark.octagon.fill",
            rowColor: .red, rowText: "Extension status unavailable")
    }
}

// MARK: - Window line presentation

/// A single status line: an SF Symbol, its tint, and the message.
private struct StatusLine {
    var symbol: String
    var color: Color
    var text: String
}

private func statusLine(_ health: Health) -> StatusLine {
    switch health {
    case .ready:
        return StatusLine(symbol: "checkmark.circle.fill", color: .green,
                          text: "Extension enabled and ready.")
    case .disabled:
        return StatusLine(symbol: "exclamationmark.triangle.fill", color: .orange,
                          text: "Extension installed but not enabled. Open System Settings to enable it.")
    case .notInstalled:
        return StatusLine(symbol: "exclamationmark.triangle.fill", color: .orange,
                          text: "Extension not registered yet — give it a moment, or relaunch the app.")
    case .error(let message):
        return StatusLine(symbol: "xmark.octagon.fill", color: .red,
                          text: "Couldn't read extension status: \(message)")
    }
}

/// The build-freshness line, or `nil` when it doesn't apply.
private func freshnessLine(_ freshness: Freshness) -> StatusLine? {
    switch freshness {
    case .unknown:
        return nil
    case .match(let sha, let dirty):
        if dirty {
            return StatusLine(symbol: "checkmark.seal", color: .yellow,
                              text: "Extension build matches this app (\(sha)) — dirty build, so equal SHAs aren't a guarantee.")
        }
        return StatusLine(symbol: "checkmark.seal.fill", color: .green,
                          text: "Extension build matches this app (\(sha)).")
    case .mismatch(let registered, let host):
        // When the extension is enabled this is the *running* build (the /_info probe);
        // otherwise it's the build fskitd would launch (the bundle on disk).
        return StatusLine(
            symbol: "exclamationmark.triangle.fill", color: .orange,
            text: """
                Build mismatch — a DIFFERENT extension build than this app.
                Extension: \(registered)
                This app: \(host)
                Relaunch to re-register; if it persists: sudo killall fskitd
                """)
    }
}

/// A line describing the launch-at-login registration state.
private func autostartLine(_ status: Status) -> String {
    switch status {
    case .enabled:
        return "Launch at login: on."
    case .requiresApproval:
        return "Launch at login: awaiting your approval in System Settings ▸ Login Items."
    case .notRegistered:
        return "Launch at login: off."
    case .notFound, .unknown:
        return "Launch at login: unavailable (unsigned/dev build)."
    }
}

// MARK: - The window

struct HealthView: View {
    @Environment(AppModel.self) private var model
    @State private var autostart: Status = .unknown

    var body: some View {
        VStack(spacing: 0) {
            HealthLines(report: model.report, autostart: autostart)
                .frame(maxWidth: .infinity, alignment: .topLeading)
                .padding(20)

            Divider()

            HStack {
                Button("Re-check") { Task { await refresh() } }
                Spacer()
                Button("Open System Settings…") { openExtensionsSettings() }
                    .keyboardShortcut(.defaultAction)
                    .glassProminentButtonStyle()
            }
            .glassButtonStyle()
            .padding(.horizontal, 24)
            .padding(.vertical, 12)
        }
        .frame(width: 480)
        .sizeWindowToContent(title: "fskit-s3 Extension")
        .glassWindowBackground()
        .task { await refresh() }
    }

    /// Re-check the extension health and the login-item status together.
    private func refresh() async {
        guard !isRunningInPreview else { return }
        await model.refresh()
        autostart = await Task.detached { autostartStatus() }.value
    }
}

/// The informational lines of the health window, pure (state in → view out) so it
/// renders in a `#Preview` without touching the contract.
struct HealthLines: View {
    let report: Report
    let autostart: Status

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            line(statusLine(report.health))

            if let fresh = freshnessLine(report.freshness) {
                line(fresh)
            }

            Text(autostartLine(autostart))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// One symbol + wrapping text line.
    private func line(_ status: StatusLine) -> some View {
        Label {
            Text(status.text)
                .fixedSize(horizontal: false, vertical: true)
        } icon: {
            Image(systemName: status.symbol)
                .foregroundStyle(status.color)
        }
    }
}

/// Open **System Settings ▸ Login Items & Extensions ▸ File System Extensions** —
/// where the user enables the extension (macOS won't let the app flip that toggle).
private func openExtensionsSettings() {
    if let url = URL(string: "x-apple.systempreferences:com.apple.ExtensionsPreferences") {
        NSWorkspace.shared.open(url)
    }
}

#Preview("Ready") {
    HealthLines(
        report: Report(health: .ready, freshness: .match(sha: "a1b2c3d", dirty: false)),
        autostart: .enabled
    )
    .frame(width: 460, alignment: .leading)
    .padding(20)
}

#Preview("Not enabled") {
    HealthLines(
        report: Report(health: .disabled, freshness: .unknown),
        autostart: .notRegistered
    )
    .frame(width: 460, alignment: .leading)
    .padding(20)
}

#Preview("Build mismatch") {
    HealthLines(
        report: Report(
            health: .ready,
            freshness: .mismatch(registered: "a1b2c3d", host: "e4f5a6b-dirty")),
        autostart: .requiresApproval
    )
    .frame(width: 460, alignment: .leading)
    .padding(20)
}
