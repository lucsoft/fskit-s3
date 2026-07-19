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

    var body: some View {
        let mounted = model.isMounted(connection.name)

        Menu {
            if mounted {
                Button("Unmount") { unmountAction() }
            } else {
                Button("Mount") { mountAction() }
            }
            Button("Update…") {
                activateAndOpen {
                    openWindow(value: ConnectionFormRequest(originalName: connection.name))
                }
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
