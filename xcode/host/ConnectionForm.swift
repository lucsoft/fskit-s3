//  ConnectionForm.swift
//  The connection editor + the secret prompt — native SwiftUI `Form`s.
//
//  This replaces the hand-laid-out NSWindow form (the old addwindow.rs), which
//  positioned every control by pixel rect. Here it's a grouped `Form`: sections,
//  labelled rows, a `Picker`, `Toggle`s, and a `SecureField`, all auto-laid-out
//  and theme-aware. "Test & Save" runs the S3 credential check off the main actor
//  through the contract (saveConnection), so the UI never blocks on the network —
//  which also closes the old "move Test & Save off the main thread" to-do for free.

import SwiftUI

/// New-connection / edit-connection form. `request.originalName == nil` creates a
/// new connection; a name edits that one (its name field locked, fields pre-filled).
struct ConnectionFormView: View {
    let request: ConnectionFormRequest

    @Environment(\.dismiss) private var dismiss
    @Environment(AppModel.self) private var model

    @State private var name = ""
    @State private var isS3 = false
    @State private var endpoint = ""
    @State private var bucket = ""
    @State private var region = ""
    @State private var accessKeyId = ""
    @State private var secret = ""
    @State private var sessionToken = ""
    @State private var saveToKeychain = false
    @State private var mountOnLaunch = false

    @State private var status = ""
    @State private var busy = false
    @State private var loaded = false

    private var isEditing: Bool { request.originalName != nil }

    var body: some View {
        VStack(spacing: 0) {
            Form {
                Section {
                    TextField("Name", text: $name, prompt: Text("Required"))
                        .disabled(isEditing) // the name is the registry key — locked on edit
                    Picker("Type", selection: $isS3) {
                        Text("In-memory").tag(false)
                        Text("S3").tag(true)
                    }
                }

                if isS3 {
                    Section("S3") {
                        TextField("Endpoint", text: $endpoint,
                                  prompt: Text("https://s3.example.com"))
                        TextField("Bucket", text: $bucket, prompt: Text("Required"))
                        TextField("Region", text: $region, prompt: Text("auto"))
                        TextField("Access Key ID", text: $accessKeyId, prompt: Text("Required"))
                        SecureField("Secret", text: $secret, prompt: Text("Required"))
                        TextField("Session token", text: $sessionToken, prompt: Text("Optional"))
                        Toggle("Save secret to Keychain", isOn: $saveToKeychain)
                    }
                }

                Section {
                    Toggle("Mount when launching", isOn: $mountOnLaunch)
                }

                if !status.isEmpty {
                    Section {
                        Text(status)
                            .foregroundStyle(.red)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)

            Divider()

            // Bottom button bar (the classic macOS dialog layout), Liquid Glass.
            HStack {
                if isEditing {
                    Button("Delete", role: .destructive) { delete() }
                        .disabled(busy)
                }
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                if busy {
                    ProgressView().controlSize(.small).padding(.horizontal, 4)
                }
                Button("Test & Save") { save() }
                    .keyboardShortcut(.defaultAction)
                    .glassProminentButtonStyle()
                    .disabled(busy)
            }
            .glassButtonStyle()
            .padding(.horizontal, 24)
            .padding(.vertical, 12)
        }
        .frame(width: 480)
        .sizeWindowToContent(title: isEditing ? "Edit Connection" : "New Connection")
        .glassWindowBackground()
        .task { await populate() }
    }

    /// Pre-fill the fields when editing (once). Reads run off the main actor.
    private func populate() async {
        guard let original = request.originalName, !loaded else { return }
        loaded = true
        let connection = await Task.detached { listConnections().first { $0.name == original } }.value
        guard let connection else { return }
        name = connection.name
        saveToKeychain = connection.saveSecretToKeychain
        mountOnLaunch = connection.mountOnLaunch
        if case .s3(let meta) = connection.kind {
            isS3 = true
            endpoint = meta.endpoint
            bucket = meta.bucket
            region = meta.region
            accessKeyId = meta.accessKeyId
            sessionToken = meta.sessionToken ?? ""
        }
        secret = await Task.detached { readSecret(name: original) }.value ?? ""
    }

    private func currentForm() -> FormInput {
        FormInput(
            name: name, isS3: isS3, endpoint: endpoint, bucket: bucket, region: region,
            accessKeyId: accessKeyId, secret: secret, sessionToken: sessionToken,
            saveSecretToKeychain: saveToKeychain, mountOnLaunch: mountOnLaunch)
    }

    private func save() {
        busy = true
        status = ""
        let form = currentForm()
        let original = request.originalName
        Task {
            do {
                // saveConnection lists the S3 bucket to verify credentials, so run it
                // off the main actor — the UI stays responsive during the check.
                _ = try await Task.detached { try saveConnection(form: form, originalName: original) }.value
                await model.refreshConnections()
                dismiss()
            } catch let error as FfiError {
                status = ffiMessage(error)
            } catch {
                status = "\(error)"
            }
            busy = false
        }
    }

    private func delete() {
        guard let original = request.originalName else { return }
        guard confirmDestructive(
            "Delete the connection “\(original)”?",
            "This unmounts it if mounted, then removes its configuration and stored secret. This can't be undone.",
            confirmTitle: "Delete"
        ) else { return }

        busy = true
        status = ""
        Task {
            do {
                try await Task.detached { try deleteConnection(name: original) }.value
                await model.refreshConnections()
                dismiss()
            } catch let error as FfiError {
                status = ffiMessage(error)
            } catch {
                status = "\(error)"
            }
            busy = false
        }
    }
}

/// Prompt for an S3 connection's secret, then mount it (storing the secret in the
/// Keychain if asked). Shown when a mount is requested but no secret is stored.
struct SecretPromptView: View {
    let name: String

    @Environment(\.dismiss) private var dismiss
    @Environment(AppModel.self) private var model

    @State private var secret = ""
    @State private var saveToKeychain = true
    @State private var status = ""
    @State private var busy = false

    var body: some View {
        VStack(spacing: 0) {
            Form {
                Section {
                    Text("Enter the S3 secret access key for “\(name)”.")
                        .fixedSize(horizontal: false, vertical: true)
                    SecureField("Secret access key", text: $secret)
                    Toggle("Save to Keychain", isOn: $saveToKeychain)
                }
                if !status.isEmpty {
                    Section {
                        Text(status)
                            .foregroundStyle(.red)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)

            Divider()

            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                if busy {
                    ProgressView().controlSize(.small).padding(.horizontal, 4)
                }
                Button("Mount") { submit() }
                    .keyboardShortcut(.defaultAction)
                    .glassProminentButtonStyle()
                    .disabled(secret.isEmpty || busy)
            }
            .glassButtonStyle()
            .padding(.horizontal, 20)
            .padding(.vertical, 16)
        }
        .frame(width: 420)
        .sizeWindowToContent(title: "Secret for \(name)")
        .glassWindowBackground()
    }

    private func submit() {
        busy = true
        status = ""
        let enteredSecret = secret
        let save = saveToKeychain
        Task {
            do {
                try await Task.detached {
                    try mountWithSecret(name: name, secret: enteredSecret, saveToKeychain: save)
                }.value
                await model.refreshConnections()
                dismiss()
            } catch let error as FfiError {
                status = ffiMessage(error)
            } catch {
                status = "\(error)"
            }
            busy = false
        }
    }
}

// New-connection form. It loads no data (populate early-returns for a nil request),
// and the preview is interactive — switch Type to S3 to reveal the S3 section.
#Preview("New connection") {
    ConnectionFormView(request: ConnectionFormRequest(originalName: nil))
        .environment(AppModel())
}

#Preview("Secret prompt") {
    SecretPromptView(name: "photos")
        .environment(AppModel())
}
