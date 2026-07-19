//  Support.swift
//  Small shared helpers for the SwiftUI app: modal alerts, window-open requests,
//  and a couple of contract-type mappings.
//
//  Modal alerts use AppKit's NSAlert rather than SwiftUI's `.alert`: this is a
//  menu-bar (accessory) app, and presenting a SwiftUI alert from a menu — which
//  closes as it's clicked — is unreliable, whereas a run-loop-modal NSAlert is
//  exactly what the old Rust UI used. Everything here is main-actor UI work.

import SwiftUI
import AppKit

/// The value that opens the connection form window. `originalName == nil` means
/// "new connection"; a name means "edit that connection" (its name locked).
/// Codable + Hashable so it can drive a `WindowGroup(for:)` scene.
struct ConnectionFormRequest: Codable, Hashable {
    var originalName: String?
}

/// The value that opens the secret-prompt window (mount an S3 connection whose
/// secret isn't stored).
struct SecretPromptRequest: Codable, Hashable {
    var name: String
}

/// Reads the `NSWindow` hosting a SwiftUI view, so a view can drive window-level
/// behaviour (here: an animated resize and the window title). Attach with
/// `.background(WindowReader { window in ... })`. The closure may fire more than
/// once — guard one-time work with your own flag.
struct WindowReader: NSViewRepresentable {
    var onWindow: (NSWindow) -> Void

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        // The view isn't in a window yet inside makeNSView; defer to the next runloop.
        DispatchQueue.main.async { if let window = view.window { onWindow(window) } }
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        DispatchQueue.main.async { if let window = nsView.window { onWindow(window) } }
    }
}

/// Makes the content hug its own height and sets the window title, so the scene's
/// `.windowResizability(.contentSize)` can size the window to fit — no empty space,
/// grows and shrinks with the layout (e.g. the connection form In-memory ↔ S3), and
/// no user-resizing. A grouped `Form` is greedy vertically, so `.fixedSize` is what
/// lets it report a compact height for the window to adopt.
///
/// (An earlier version drove `NSWindow.setFrame(…, animate:)` by hand to animate the
/// resize; animating a non-opaque window from the geometry callback reentered layout
/// and crashed — letting SwiftUI own the size is stable.)
private struct SizeWindowToContent: ViewModifier {
    let title: String

    func body(content: Content) -> some View {
        content
            .fixedSize(horizontal: false, vertical: true)
            // Hide the toolbar's own material background so the window's glass shows
            // through the unified bar too (the toolbar paints a separate background
            // that `titlebarAppearsTransparent` doesn't remove).
            .toolbarBackground(.hidden, for: .windowToolbar)
            .background(
                WindowReader { window in
                    window.title = title
                    // Keep the tall unified title bar even though the action buttons
                    // live in a bottom bar (so there are no SwiftUI `.toolbar` items to
                    // create a toolbar). An empty NSToolbar makes the unified bar — and
                    // the window title in it — render; the scene's `.windowToolbarStyle`
                    // styles it. Re-applied on updates so SwiftUI can't drop it.
                    if window.toolbar == nil {
                        window.toolbar = NSToolbar()
                    }
                    window.toolbarStyle = .unified
                }
            )
    }
}

extension View {
    /// Hug the content's height (so the window's `.contentSize` resizability fits it)
    /// and set the window title. See `SizeWindowToContent`.
    func sizeWindowToContent(title: String) -> some View {
        modifier(SizeWindowToContent(title: title))
    }
}

private struct GlassWindowBackground: ViewModifier {
    // Liquid Glass is macOS 26+; earlier systems just get the plain window.
    @ViewBuilder
    func body(content: Content) -> some View {
        if #available(macOS 26.0, *) {
            content
                // The real Liquid Glass material, full-bleed behind the content.
                .background(
                    Color.clear
                        .glassEffect(.regular, in: Rectangle())
                        .ignoresSafeArea()
                )
                //.buttonStyle(.glassProminent)
                // The glass refracts what's behind the window, so the window must be
                // non-opaque + clear. Also make the title bar transparent and let the
                // content view span the full height, so the glass background extends
                // *under* the unified toolbar — otherwise the toolbar keeps its own
                // opaque fill and only the content area looks like glass.
                .background(
                    WindowReader { window in
                        window.isOpaque = false
                        window.backgroundColor = .clear
                        window.titlebarAppearsTransparent = true
                        window.styleMask.insert(.fullSizeContentView)
                    }
                )
        } else {
            content
        }
    }
}

extension View {
    /// Give the hosting window a translucent Liquid Glass material background
    /// (macOS 26+; a no-op on earlier systems). Pair with
    /// `.scrollContentBackground(.hidden)` on a `Form` so the glass shows through
    /// the content, not just the margins.
    func glassWindowBackground() -> some View {
        modifier(GlassWindowBackground())
    }

    /// The Liquid Glass button style for the buttons in this container on macOS 26+,
    /// leaving the standard style on earlier systems.
    @ViewBuilder
    func glassButtonStyle() -> some View {
        if #available(macOS 26.0, *) {
            buttonStyle(.glass)
        } else {
            self
        }
    }

    /// The prominent (default-action) Liquid Glass button style on macOS 26+,
    /// falling back to `.borderedProminent`.
    @ViewBuilder
    func glassProminentButtonStyle() -> some View {
        if #available(macOS 26.0, *) {
            buttonStyle(.glassProminent)
        } else {
            buttonStyle(.borderedProminent)
        }
    }
}

/// True inside an Xcode SwiftUI preview, so views can skip live contract calls
/// (a health XPC query, a `/sbin/mount` shell-out) that would make the canvas slow
/// or side-effecting. Previews render from injected sample state instead.
var isRunningInPreview: Bool {
    ProcessInfo.processInfo.environment["XCODE_RUNNING_FOR_PREVIEWS"] == "1"
}

/// The message to show for a contract error.
func ffiMessage(_ error: FfiError) -> String {
    switch error {
    case .Message(let message): return message
    case .NeedsSecret: return "This connection needs its S3 secret access key."
    }
}

/// A short human label for a connection kind (menu subtitle).
func kindLabel(_ kind: ConnectionKind) -> String {
    switch kind {
    case .memory: return "in-memory demo"
    case .s3: return "S3"
    }
}

/// Whether a connection kind is S3 (needs a secret to mount).
func isS3(_ kind: ConnectionKind) -> Bool {
    if case .s3 = kind { return true }
    return false
}

/// Bring the app forward, then run `open` — an accessory app doesn't auto-activate,
/// so a window opened without this can appear behind other apps.
@MainActor
func activateAndOpen(_ open: () -> Void) {
    NSApplication.shared.activate(ignoringOtherApps: true)
    open()
}

/// Present an open panel to pick a single folder (creating one is allowed). Returns
/// the chosen path, or `nil` if cancelled. The emptiness of the folder is enforced by
/// the contract (`saveConnection`), not here.
@MainActor
func chooseMountFolder() -> String? {
    let panel = NSOpenPanel()
    panel.canChooseDirectories = true
    panel.canChooseFiles = false
    panel.canCreateDirectories = true
    panel.allowsMultipleSelection = false
    panel.prompt = "Choose"
    panel.message = "Choose an empty folder to mount this connection into."
    NSApplication.shared.activate(ignoringOtherApps: true)
    return panel.runModal() == .OK ? panel.url?.path : nil
}

/// Reveal a mount folder in Finder. If it doesn't exist yet (an unmounted default
/// path is removed on unmount), open its parent so the user still lands nearby.
@MainActor
func openInFinder(_ path: String) {
    let url = URL(fileURLWithPath: path)
    if FileManager.default.fileExists(atPath: path) {
        NSWorkspace.shared.open(url)
    } else {
        NSWorkspace.shared.open(url.deletingLastPathComponent())
    }
}

/// What the user chose in the auto-mount failure alert.
enum AutoMountFailureChoice { case retry, restartAndRetry, dismiss }

/// A modal alert offering to recover from failed auto-mounts: **Retry** (mount the
/// failed ones again), **Restart Extension & Retry** (`killall fskitd` — the reset for
/// a "Resource busy" stuck instance — then re-mount), or **Dismiss**.
@MainActor
func autoMountFailureAlert(_ title: String, _ detail: String) -> AutoMountFailureChoice {
    let alert = NSAlert()
    alert.messageText = title
    alert.informativeText = detail
    alert.alertStyle = .warning
    alert.addButton(withTitle: "Retry") // .alertFirstButtonReturn
    alert.addButton(withTitle: "Restart Extension & Retry") // .alertSecondButtonReturn
    alert.addButton(withTitle: "Dismiss") // .alertThirdButtonReturn
    NSApplication.shared.activate(ignoringOtherApps: true)
    switch alert.runModal() {
    case .alertFirstButtonReturn: return .retry
    case .alertSecondButtonReturn: return .restartAndRetry
    default: return .dismiss
    }
}

/// A modal error alert with a single OK button (mirrors the old `appkit::show_error`).
@MainActor
func showError(_ title: String, _ message: String) {
    let alert = NSAlert()
    alert.messageText = title
    alert.informativeText = message
    alert.alertStyle = .warning
    alert.addButton(withTitle: "OK")
    NSApplication.shared.activate(ignoringOtherApps: true)
    alert.runModal()
}

/// A modal "are you sure?" alert with a destructive confirm button. Returns `true`
/// only if the user clicks it (mirrors the old `appkit::confirm`).
@MainActor
func confirmDestructive(_ title: String, _ message: String, confirmTitle: String) -> Bool {
    let alert = NSAlert()
    alert.messageText = title
    alert.informativeText = message
    alert.alertStyle = .warning
    let confirm = alert.addButton(withTitle: confirmTitle)
    confirm.hasDestructiveAction = true
    alert.addButton(withTitle: "Cancel")
    NSApplication.shared.activate(ignoringOtherApps: true)
    return alert.runModal() == .alertFirstButtonReturn
}
