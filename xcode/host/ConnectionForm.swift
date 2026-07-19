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
    @State private var saveToDisk = false
    @State private var mountOnLaunch = false
    /// The custom mount folder (empty ⇒ the default `~/fskit-s3/<name>`). Chosen via
    /// an open panel; editable on create and edit.
    @State private var mountPoint = ""
    @FocusState private var secretFocused: Bool

    /// A non-typable placeholder loaded into the Secret field when a stored secret
    /// exists (it renders as dots, signalling "a password is set" without the secret
    /// ever crossing back from Rust). Leaving it untouched keeps the stored secret;
    /// focusing the field clears it so the user starts fresh — a *blank* field then
    /// means an empty secret, not "keep". Chosen so a real secret can't equal it.
    private let storedSecretMarker = "\u{2063}\u{2063}fskit-s3.stored-secret\u{2063}\u{2063}"

    /// Whether the Secret field still holds the untouched stored-secret placeholder.
    private var keepsStoredSecret: Bool { secret == storedSecretMarker }

    @State private var status = ""
    @State private var busy = false
    @State private var loaded = false

    private var isEditing: Bool { request.originalName != nil }

    /// What the default mount folder will be, given the name typed so far.
    private var defaultMountHint: String {
        "~/fskit-s3/\(name.isEmpty ? "<name>" : name)"
    }

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
                        TextField("Region", text: $region, prompt: Text("us-east-1"))
                        TextField("Access Key ID", text: $accessKeyId, prompt: Text("Required"))
                        SecureField("Secret", text: $secret,
                                    prompt: Text(isEditing ? "New secret (blank = none)" : "Required"))
                            .focused($secretFocused)
                            .onChange(of: secretFocused) { _, focused in
                                // Focusing the placeholder clears it, so the user types a
                                // fresh secret; leaving it unfocused keeps the stored one.
                                if focused && keepsStoredSecret { secret = "" }
                            }
                        TextField("Session token", text: $sessionToken, prompt: Text("Optional"))
                        Toggle("Save secret to Keychain", isOn: $saveToKeychain)
                        Toggle(isOn: $saveToDisk) {
                            Text("Save secret to disk (dev)")
                            Text("Plaintext, for unsigned builds where the extension can't read the Keychain. Insecure — the secret is stored in the clear.")
                        }
                    }
                }

                Section {
                    Toggle("Mount when launching", isOn: $mountOnLaunch)
                }

                // The mount folder — pick an empty folder to mount into, or leave it for
                // the default. Editable on create and edit alike (Open in Finder lives
                // in the menu bar, not here).
                Section {
                    LabeledContent("Mount folder") {
                        HStack {
                            Text(mountPoint.isEmpty ? defaultMountHint : mountPoint)
                                .foregroundStyle(mountPoint.isEmpty ? .secondary : .primary)
                                .lineLimit(1)
                                .truncationMode(.middle)
                            if !mountPoint.isEmpty {
                                Button("Default") { mountPoint = "" }
                            }
                            Button("Choose…") {
                                if let picked = chooseMountFolder() { mountPoint = picked }
                            }
                        }
                    }
                } footer: {
                    Text("Pick an empty folder, or leave it to use \(defaultMountHint).")
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
        saveToDisk = connection.saveSecretToDisk
        mountOnLaunch = connection.mountOnLaunch
        // The raw stored value (empty ⇒ default), so saving an unchanged folder keeps it.
        mountPoint = connection.mountPoint ?? ""
        if case .s3(let meta) = connection.kind {
            isS3 = true
            endpoint = meta.endpoint
            bucket = meta.bucket
            region = meta.region
            accessKeyId = meta.accessKeyId
            sessionToken = meta.sessionToken ?? ""
        }
        // The secret never crosses back to Swift. If one is stored, load the dots
        // placeholder so the field shows a secret exists; leaving it untouched keeps it.
        if await Task.detached(operation: { hasSecret(name: original) }).value {
            secret = storedSecretMarker
        }
    }

    private func currentForm() -> FormInput {
        // Untouched placeholder ⇒ keep the stored secret; never send the marker itself.
        let keep = keepsStoredSecret
        return FormInput(
            name: name, isS3: isS3, endpoint: endpoint, bucket: bucket, region: region,
            accessKeyId: accessKeyId, secret: keep ? "" : secret, sessionToken: sessionToken,
            keepStoredSecret: keep,
            saveSecretToKeychain: saveToKeychain, saveSecretToDisk: saveToDisk,
            mountOnLaunch: mountOnLaunch, mountPoint: mountPoint)
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
    @State private var saveToDisk = false
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
                    Toggle(isOn: $saveToDisk) {
                        Text("Save to disk (dev)")
                        Text("Plaintext fallback for unsigned builds. Insecure.")
                    }
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
        let saveDisk = saveToDisk
        Task {
            do {
                try await Task.detached {
                    try mountWithSecret(
                        name: name, secret: enteredSecret,
                        saveToKeychain: save, saveToDisk: saveDisk)
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
