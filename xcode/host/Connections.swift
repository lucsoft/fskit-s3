//  Connections.swift
//  The connections part of the menu: one submenu per configured connection with a
//  Mount/Unmount toggle and an Update… action, plus a mounted/not-mounted glyph.
//
//  Actions call the UniFFI contract (mountConnection / unmount / …) and refresh the
//  model so the menu reflects the new state next time it opens. Errors surface as
//  NSAlerts; a mount that needs a secret opens the secret-prompt window instead.

import SwiftUI
import AppKit

/// A coloured status dot for a menu row — a **non-template** tinted `NSImage`, so the
/// menu renders it in colour (a symbol tinted with `.foregroundStyle` is treated as a
/// template and comes out monochrome in a menu). Green filled = mounted, grey hollow
/// = not.
private func statusDot(mounted: Bool) -> NSImage {
    let symbol = mounted ? "circle.fill" : "circle"
    let color: NSColor = mounted ? .systemGreen : .tertiaryLabelColor
    let base = NSImage(systemSymbolName: symbol, accessibilityDescription: nil) ?? NSImage()
    let tinted = base.withSymbolConfiguration(.init(paletteColors: [color])) ?? base
    tinted.isTemplate = false
    return tinted
}

/// The "Connections" section: a titled group of per-connection submenus.
struct ConnectionsMenu: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Section("Connections") {
            ForEach(model.connections, id: \.name) { connection in
                ConnectionMenu(connection: connection)
            }
        }
    }
}

/// One connection's submenu.
private struct ConnectionMenu: View {
    let connection: Connection
    @Environment(AppModel.self) private var model
    @Environment(\.openWindow) private var openWindow

    /// Where this connection mounts — its custom folder, else the default
    /// `~/fskit-s3/<name>`. Computed in Swift (no FFI in `body`); mirrors the Rust
    /// `Connection::mount_point` / `base_dir`.
    private var mountPath: String {
        if let custom = connection.mountPoint, !custom.isEmpty { return custom }
        return (NSHomeDirectory() as NSString).appendingPathComponent("fskit-s3/\(connection.name)")
    }

    var body: some View {
        let mounted = model.isMounted(connection.name)

        Menu {
            if mounted {
                Button { unmountAction() } label: {
                    Label("Unmount", systemImage: "eject.fill")
                }
            } else {
                Button { mountAction() }  label: {
                    Label("Mount", systemImage: "mount")
                }
            }
            // The mount folder + a shortcut to reveal it in Finder.
            Text(mountPath)
            Button {
                openInFinder(mountPath)
            } label: {
                Label("Open in Finder", systemImage: "folder")
            }
            Divider()
            Button {
                activateAndOpen {
                    openWindow(value: ConnectionFormRequest(originalName: connection.name))
                }
            } label: {
                Label("Configure...", systemImage: "gear")
            }
        } label: {
            Label {
                Text("\(connection.name)  (\(kindLabel(connection.kind)))")
            } icon: {
                Image(nsImage: statusDot(mounted: mounted))
            }
        }
    }

    private func mountAction() {
        Task {
            do {
                try mountConnection(name: connection.name)
                await model.refreshConnections()
            } catch let error as FfiError {
                switch error {
                case .NeedsSecret:
                    activateAndOpen {
                        openWindow(value: SecretPromptRequest(name: connection.name))
                    }
                case .Message(let message):
                    showError("Couldn't mount “\(connection.name)”", message)
                }
            } catch {
                showError("Couldn't mount “\(connection.name)”", "\(error)")
            }
        }
    }

    private func unmountAction() {
        Task {
            do {
                try unmount(mountPoint: mountPointFor(name: connection.name))
                await model.refreshConnections()
            } catch {
                showError("Couldn't unmount “\(connection.name)”", "\(error)")
            }
        }
    }
}
