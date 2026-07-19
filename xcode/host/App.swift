//  App.swift
//  The fskit-s3 menu-bar app — native SwiftUI on top of the Rust contract.
//
//  This target used to hand control straight to the Rust AppKit UI
//  (`fskit_s3_app_run`). It's now a SwiftUI `MenuBarExtra` app: all UI is Swift,
//  and everything it *does* — health checks, the connection registry, Keychain,
//  mounting — goes through the UniFFI contract in Generated/fskit_s3_app.swift
//  (Rust: app/src/ffi.rs). The target still links libfskit_s3_app.a (which holds
//  both the contract and the embedded FSKit extension's host duties).

import SwiftUI
import Observation
import AppKit

/// Observable app state shared by the scenes: the latest health report plus the
/// connections and the live fskit mounts (joined to show a mounted/not-mounted dot).
/// Heavy calls run off the main actor so the UI never blocks.
@Observable
final class AppModel {
    /// The most recent extension-health report. Starts as a "checking…" placeholder,
    /// replaced by the first `refresh()`.
    var report = Report(health: .error("checking…"), freshness: .unknown)
    /// The configured connections (registry order).
    var connections: [Connection] = []
    /// The mounts this filesystem currently serves.
    var mounts: [Mount] = []

    /// Re-run the health check on a background task and apply it on the main actor.
    @MainActor
    func refresh() async {
        report = await Task.detached(priority: .userInitiated) { checkHealth() }.value
    }

    /// Reload connections + live mounts (both off the main actor — listing mounts
    /// shells out to `/sbin/mount`).
    @MainActor
    func refreshConnections() async {
        async let connections = Task.detached { listConnections() }.value
        async let mounts = Task.detached { listFskitMounts() }.value
        self.connections = await connections
        self.mounts = await mounts
    }

    /// Whether the named connection is mounted at its default mount point.
    func isMounted(_ name: String) -> Bool {
        let point = mountPointFor(name: name)
        return mounts.contains { $0.mountPoint == point }
    }

    /// Best-effort launch-at-login registration (no-op if already enabled). Runs
    /// off the main actor — it's a quick ServiceManagement call, but not UI work.
    func registerLoginItem() {
        Task.detached { enableAutostart() }
    }
}

/// Launch/quit hook: an accessory (menu-bar) app has no window at launch, so this
/// is where one-time startup work runs. It also owns the single `AppModel` the
/// scenes observe (the delegate is the one object SwiftUI constructs for us up front).
final class AppDelegate: NSObject, NSApplicationDelegate {
    let model = AppModel()

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Xcode launches the real app process to host previews, which fires this
        // delegate — so skip the launch side effects (login-item registration,
        // auto-mount) when previewing.
        guard !isRunningInPreview else { return }
        model.registerLoginItem()
        Task {
            await model.refresh()
            // Auto-mount the flagged connections, then reflect the new mount state.
            await Task.detached { _ = autoMountOnLaunch() }.value
            await model.refreshConnections()
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        // Cleanly unmount our volumes so fskitd isn't left with an orphaned record.
        unmountAllOnQuit()
    }
}

@main
struct FskitS3App: App {
    @NSApplicationDelegateAdaptor private var delegate: AppDelegate

    var body: some Scene {
        // Xcode hosts previews by launching the real app process, so this scene tree
        // is built even while a #Preview shows a single view — which would otherwise
        // add a live menu-bar item to your Mac. SceneBuilder can't branch, so instead
        // of omitting the scene we just don't *insert* the status item in previews
        // (`isInserted`). The window scenes below don't auto-open, so nothing else
        // shows. Launch side effects are guarded separately in the AppDelegate.
        MenuBarExtra(isInserted: .constant(!isRunningInPreview)) {
            MenuContent()
                .environment(delegate.model)
        } label: {
            Image(systemName: presentation(for: delegate.model.report).barSymbol)
        }

        // The extension-health window, opened from the menu's health row.
        Window("fskit-s3 Extension", id: HealthWindow.id) {
            HealthView()
                .environment(delegate.model)
        }
        .windowResizability(.contentSize)
        .windowToolbarStyle(.unified(showsTitle: true))

        // The connection editor — one window per request value (nil = new).
        WindowGroup("Connection", id: "connection", for: ConnectionFormRequest.self) { $request in
            ConnectionFormView(request: request ?? ConnectionFormRequest(originalName: nil))
                .environment(delegate.model)
        }
        .windowResizability(.contentSize)
        .windowToolbarStyle(.unified(showsTitle: true))

        // The secret prompt, opened when mounting an S3 connection with no stored secret.
        WindowGroup("Secret", id: "secret", for: SecretPromptRequest.self) { $request in
            if let request {
                SecretPromptView(name: request.name)
                    .environment(delegate.model)
            }
        }
        .windowResizability(.contentSize)
        .windowToolbarStyle(.unified(showsTitle: true))
    }
}

/// The dropdown shown from the menu-bar item. Rebuilt each time the menu opens, so
/// `.task` re-runs the health + connection refresh and the rows reflect the latest
/// state.
struct MenuContent: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        let p = presentation(for: model.report)

        Group {
            ConnectionsMenu()

            Divider()

            Button {
                activateAndOpen {
                    openWindow(value: ConnectionFormRequest(originalName: nil))
                }
            } label: {
                Label("New Connection…", systemImage: "plus")
            }

            Divider()

            // Extension health — clicking opens the health window.
            Button {
                activateAndOpen { openWindow(id: HealthWindow.id) }
            } label: {
                Label(p.rowText, systemImage: p.rowSymbol)
            }

            Button {
                NSApplication.shared.terminate(nil)
            } label: {
                Label("Quit fskit-s3", systemImage: "xmark")
            }
            .keyboardShortcut("q")
        }
        .task {
            guard !isRunningInPreview else { return }
            async let health: Void = model.refresh()
            async let connections: Void = model.refreshConnections()
            _ = await (health, connections)
        }
    }
}
