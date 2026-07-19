# Building the FSKit extension

The Rust `ext` crate is the whole file system (bindings, `FSUnaryFileSystem` /
`FSVolume` / `FSItem` subclasses, read path). Xcode only provides the
ExtensionKit packaging, signing, and the ~8-line Swift `@main` bootstrap
([FileSystemExtension.swift](FileSystemExtension.swift)), which just returns the
Rust class via `fskit_s3_make_filesystem`.

The project is generated from [`../project.yml`](../project.yml) by **xcodegen**
(no `.xcodeproj` is checked in). It already compiles + links end-to-end
(verified with `CODE_SIGNING_ALLOWED=NO`); only signing is left.

## Generate + open

```sh
brew install xcodegen          # once
xcodegen generate              # -> fskit-s3.xcodeproj
open fskit-s3.xcodeproj
```

## Sign with the BBN team

`project.yml` sets `DEVELOPMENT_TEAM = H8563U643B` and automatic signing. In
Xcode, select each target ▸ **Signing & Capabilities** and confirm the **BBN**
team is chosen.

The extension declares the FSKit Module capability in
[`Extension.entitlements`](Extension.entitlements). If Xcode reports the
provisioning profile is missing `com.apple.developer.fskit.fsmodule`:

1. developer.apple.com ▸ Certificates, Identifiers & Profiles ▸ **Identifiers** ▸
   the App ID `dev.lucsoft.fskit-s3.ext` (Xcode creates it on first build) ▸
   enable **FSKit Module**.
2. Back in Xcode, let automatic signing regenerate the profile.

This needs an **Admin/App Manager** role on the BBN team, and a **paid**
membership (the capability isn't available to free teams).

Both targets also share a **Keychain access group** (`keychain-access-groups` in
both entitlements files) so the app can hand the extension an S3 secret. Enable
**Keychain Sharing** on both App IDs (`dev.lucsoft.fskit-s3` and
`dev.lucsoft.fskit-s3.ext`) the same way — Identifiers ▸ each App ID ▸ **Keychain
Sharing** — or add the *Keychain Sharing* capability to each target in Xcode ▸
Signing & Capabilities and let automatic signing regenerate the profiles.

## Build, install, mount

1. Scheme **fskit-s3-host** ▸ **Build & Run** (this signs + registers the extension).
   The app is a ☁ **status-bar** app; its top menu row is a **live health check**
   (via `FSClient`, in Rust) showing whether the extension is installed + enabled,
   and it registers itself to launch at login. When the extension isn't ready it
   pops a health window automatically.
2. Enable it: click **Open System Settings…** in the health window (its top menu
   row opens it too), or **System Settings ▸ General ▸ Login Items & Extensions ▸
   File System Extensions** ▸ turn on *fskit-s3*. **Re-check** flips it to ✓ once
   it's on; close the window — the extension runs on its own.
3. Mount the demo volume. We declare `FSSupportsPathURLs`, so the **source** is a
   path — and it carries the config (`/memory` selects the demo; it needn't exist
   on disk):
   ```sh
   mkdir -p /tmp/fskit-s3
   mount -F -t fskit-s3 /memory /tmp/fskit-s3
   ls /tmp/fskit-s3             # -> photos/  readme.txt
   cat /tmp/fskit-s3/readme.txt # -> mounted by fskit-s3
   ```

## If it won't load — triage (the macOS 26 risk)

Third-party FSKit was broken on macOS 26.1/26.2 by an Apple bug (`fskitd`
rejecting third-party clients). Status on 26.5.x is unconfirmed. Open
**Console.app**, filter on `fskitd`, and reproduce the mount:

- **`Hello FSClient! entitlement no` / "Failed to start instance"** → the OS bug,
  not our code. Nothing in this project fixes it. Worth trying with **AMFI off**
  (`sudo nvram boot-args="amfi_get_out_of_my_way=1"` + reboot; SIP is already
  off) since AMFI is what enforces the entitlement check.
- **Any other error** (class not found, a crash, a specific op failing) → our
  code; capture the log and it's fixable.

Command-line capture:

```sh
log stream --predicate 'process == "fskitd" OR process == "fskit-s3-ext"' --info &
mount -F -t fskit-s3 /memory /tmp/fskit-s3
```
