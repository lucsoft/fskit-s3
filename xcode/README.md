# Building the FSKit extension in Xcode

The Rust `ext` crate is the whole file system (bindings, `FSUnaryFileSystem` /
`FSVolume` / `FSItem` subclasses, read path). Xcode only provides the
ExtensionKit packaging, signing, and the ~8-line Swift `@main` bootstrap. This
directory holds the non-Rust pieces; you assemble them into an Xcode project
once.

> **Prerequisite:** the FSKit Module capability
> (`com.apple.developer.fskit.fsmodule`) generally requires a **paid Apple
> Developer Program** membership. A free Personal Team usually can't add it — if
> Xcode won't let you add the capability, that's the blocker, not the code.

## One-time setup

1. **New host app.** Xcode ▸ File ▸ New ▸ Project ▸ macOS ▸ **App**
   (`fskit-s3-host`). This is just a shell that carries the extension.
2. **Add the extension target.** File ▸ New ▸ Target ▸ macOS ▸ **File System
   Extension** (`fskit-s3-ext`). This creates a correct ExtensionKit target
   (`EXAppExtensionAttributes` → `EXExtensionPointIdentifier =
   com.apple.fskit.fsmodule`) and embeds it in the host app.
3. **Replace the template sources** in the extension target:
   - Delete the generated `*FileSystem.swift` and `FileSystemExtension.swift`.
   - Add `xcode/FileSystemExtension.swift` (the bootstrap that calls Rust).
4. **Bridging header.** Add `xcode/Extension-Bridging-Header.h`, then set the
   extension target's Build Setting **Objective-C Bridging Header** to its path.
5. **Entitlements.** Point the extension target's **Code Signing Entitlements**
   at `xcode/Extension.entitlements` (declares the FSKit Module capability).
6. **Link the Rust staticlib:**
   - Add a **Run Script** build phase to the extension target, placed **before
     "Compile Sources"**, with body:
     ```sh
     "${SRCROOT}/../scripts/build-ext-staticlib.sh"
     ```
     (adjust the relative path so it reaches this repo's `scripts/`).
   - Build Settings ▸ **Other Linker Flags**: add
     `-L${BUILT_PRODUCTS_DIR} -lfskit_s3_ext`.
   - Build Settings ▸ **Library Search Paths**: add `${BUILT_PRODUCTS_DIR}`.
7. **Signing.** Select the extension (and host) target ▸ Signing & Capabilities
   ▸ set your **Team** (automatic signing). Confirm **FSKit Module** appears as a
   capability; if not, add it (needs the paid membership noted above).

## Build, install, mount

1. **Build & Run** the host app in Xcode (this signs and registers the
   extension).
2. Enable it: **System Settings ▸ General ▸ Login Items & Extensions ▸ File
   System Extensions** ▸ turn on *fskit-s3*.
3. Mount the demo volume:
   ```sh
   mkdir -p /tmp/fskit-s3
   mount -F -t fskit-s3 dummy /tmp/fskit-s3      # or use `diskutil`/the menubar app
   ls /tmp/fskit-s3            # -> photos/  readme.txt
   cat /tmp/fskit-s3/readme.txt # -> mounted by fskit-s3
   ```
   (The demo serves the in-memory backend; the S3 backend + Keychain config come
   next.)

## If it doesn't load

- **Console.app** ▸ filter for `fskitd` or `fskit-s3` to see why the extension
  was rejected (signing, entitlement, or a class-registration issue).
- If `fskitd` never instantiates `FSKitS3FileSystem`, that's the signal to fall
  back to a fuller Swift/ObjC shim — but the Rust class should register fine via
  `fskit_s3_make_filesystem`.

## Reproducible project (optional)

Prefer a checked-in project over the click-through above? `brew install
xcodegen`, and ask and a `project.yml` can be added that wires all of the above
so `xcodegen` regenerates the `.xcodeproj`.
