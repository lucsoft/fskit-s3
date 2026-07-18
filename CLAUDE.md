# fskit-s3

Mount an S3 bucket (or any object store) as a **native macOS volume** using
Apple's **FSKit** — a userspace filesystem framework that needs **no kernel
extension** and **no security downgrade** (unlike macFUSE). Written in Rust.

```mermaid
flowchart TD
    apps["Finder / Photos / any app"] -->|POSIX VFS| fskitd["fskitd (FSKit)"]
    fskitd -->|Objective-C| ext["ext&nbsp;— objc2 FSUnaryFileSystem subclass + tokio"]
    ext -->|"async StorageBackend trait"| core["core::StorageBackend"]
    backend["backend — OpenDAL Operator"] -.implements.-> core
    backend --> s3[("S3 today")]
    backend -.->|feature flag| more[("WebDAV / SFTP / …")]
```

## The one idea to internalise

FSKit hands the extension a tiny request vocabulary — *enumerate this directory*,
*look up / get attributes of this item*, *read this byte range* — and does not
care how they're satisfied. That indifference is the seam. The **entire**
contract between "the Apple side" and "the storage side" is one trait:

```rust
#[async_trait]
pub trait StorageBackend: Send + Sync {
    async fn list(&self, dir: &str)  -> Result<Vec<Entry>, StorageError>;
    async fn stat(&self, path: &str) -> Result<Entry,      StorageError>;
    async fn read(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, StorageError>;
}
```

Everything above the trait (`ext`) is written against `Arc<dyn StorageBackend>`
and never mentions S3. Everything below it (`backend`) is one OpenDAL adapter.
Adding a storage service (WebDAV, SFTP) touches neither the FSKit glue nor the
trait — it's an OpenDAL feature flag plus, if needed, a constructor.

FSKit's ops map 1:1 onto the trait:

- `enumerateDirectory` → `list`
- `lookupItemNamed` / `getAttributes` → `stat`
- `readFromFile … offset length` → `read`

## Key decisions (and why)

- **Rust, not Swift.** FSKit is a plain Objective-C framework — its headers
  (`FSUnaryFileSystem.h`, `FSVolume.h`, …) are ObjC, with ObjC `@protocol`s and
  block-based reply handlers, and there is no `.swiftinterface`. So it's driven
  from Rust with `objc2`/`define_class!` exactly like the sibling `wayland-macos`
  project drives AppKit. No Swift shim.
- **OpenDAL, not a hand-rolled S3 client.** OpenDAL abstracts ~40 storage
  services behind one `Operator`, so signing (SigV4), XML, retries, and
  pagination are its job. This is the whole backend roadmap (S3 → WebDAV → SFTP)
  in one dependency. The `StorageBackend` trait is still kept as a thin,
  testable seam in front of it (insulation + an in-memory backend for tests).
- **Async (tokio), not blocking.** A network filesystem is latency-bound and
  Finder/Photos issue many parallel reads. The ext owns a multi-threaded tokio
  runtime; each FSKit op `spawn`s the backend future and invokes FSKit's reply
  block on completion, so no queue thread is parked on I/O. `async-trait` keeps
  the trait dyn-compatible.
- **Credentials from the macOS Keychain.** Read at `loadResource:` time, keyed
  by the resource identity — no plaintext secrets on disk, fits the
  app-extension sandbox. (`VolumeState::demo` mounts a credential-free in-memory
  volume so FSKit plumbing can be brought up before this exists.)
- **Target: a general-purpose bucket mount** (read-only first). *Not* Photos —
  see the Photos note below.

## Object-store semantics

Object stores have **no real directories**: there are only keys, and a
"directory" is any prefix keys share. Both backends model this identically —
`list` uses a non-recursive listing (OpenDAL applies the S3 `delimiter=/`) so
files come back plain and subdirectories as entries whose path ends in `/`.
`list` returns names + kinds; **`stat` is the authoritative source of size**
(listings don't reliably carry sizes across services), which also matches
FSKit's enumerate-then-getAttributes flow.

Paths crossing the trait are absolute, `/`-separated, normalized (`core::path`):
root is `"/"`, no trailing slash otherwise, no `.`/`..`. Backends convert to
object keys with `path::to_key` (no leading slash; trailing slash for a dir
prefix).

## Source map

- **`core/src/lib.rs`** — the `StorageBackend` trait, `Entry`/`EntryKind`,
  `StorageError`. Dependency-light (just `async-trait`) so it builds/tests
  anywhere.
- **`core/src/path.rs`** — absolute-path normalization + object-key helpers,
  unit-tested.
- **`core/src/mem.rs`** — `InMemoryBackend`, a flat key→bytes map with
  object-store semantics; test fixture + no-credential demo mount (feature
  `mem`).
- **`backend/src/lib.rs`** — `OpenDalBackend`: `StorageBackend` over any OpenDAL
  `Operator`; `S3Config` + `::s3()` constructor. Tested against OpenDAL's
  in-memory service; an ignored `live_s3_roundtrip` test runs against the
  `compose.yaml` RustFS.
- **`ext/`** — the FSKit extension, in Rust (`staticlib`). `sys.rs`:
  hand-written `objc2` bindings for FSKit classes + the three volume protocols.
  `item.rs`: `FSKitS3Item` (`FSItem` subclass carrying the path). `volume.rs`:
  `FSKitS3Volume` — the read path (activate/lookup/getAttributes/enumerate/read)
  against a `StorageBackend` on a tokio runtime; mutating ops reply `EROFS`.
  `lib.rs`: `FSKitS3FileSystem` (`FSUnaryFileSystem` delegate) + the exported
  `fskit_s3_make_filesystem` entry point. `loadResource` picks the backend from
  the mount's `-o` options (`backend_for`), dispatching on an explicit `type`:
  `type=s3` (secret from the shared Keychain group, else an `-o secret`) or
  `type=memory` (the demo). A missing `type` **fails the mount** — it never
  silently serves the demo.
- **`app/src/`** — `fskit-s3-app`, the macOS app (a status-bar app):
  - `connection.rs` — the `Connection`/`ConnectionKind` (`Memory` | `S3(S3Meta)`)
    model + the persisted `Registry` (`~/Library/Application Support/fskit-s3/
    connections.json`, which **never holds a secret**). `mount_options()` emits an
    S3 connection's non-secret config as `-o` pairs.
  - `keychain.rs` — the S3 secret in the Keychain (`security-framework`),
    preferring a **shared access group** the extension can read (falls back to the
    default keychain when unsigned).
  - `s3check.rs` — the "Test and Save" credential check (lists the bucket via
    `fskit-s3-backend`/OpenDAL, the same backend the extension serves with).
  - `mounts.rs` — the mount table + `mount`/`unmount` (`mount -F -t fskit-s3
    [-o …]`). No bespoke CLI — the system `mount`/`umount` are that.
  - `addwindow.rs` — the connection form (`open` = new, `open_edit` = edit an
    existing connection, pre-filled + name locked, with a red *Delete* button that
    removes it after a confirmation) + the secret-prompt window (native `NSWindow`).
  - `main.rs` + `appkit.rs` — the status-bar UI (`objc2`): *New Connection…* plus a
    submenu per connection (a green/grey status dot + *Mount*/*Unmount* toggle +
    *Update…*). All AppKit FFI stays in `appkit.rs`.

  `connection`/`keychain`/`s3check`/`mounts` are pure Rust + unit-tested.
- **`xcode/`** — the non-Rust packaging: the Swift `@main`
  `UnaryFileSystemExtension` bootstrap (returns the Rust class via
  `fskit_s3_make_filesystem`), bridging header, entitlements, and a build recipe.
  ExtensionKit requires this Swift entry; all file-system logic stays in Rust.
  `xcode/host/FskitS3HostApp.swift` is the host app (macOS requires an app to vend
  the extension) — its window is a **live health check** that queries
  `FSClient.installedExtensions` for our module's installed/enabled state and
  self-refreshes, so enabling it in Settings flips it to ✓ (macOS won't let an app
  toggle a file-system extension itself, so it deep-links to the Settings pane).
  You can close it once ready — the extension runs in its own `fskitd`-launched
  process, independent of both apps.
- **`scripts/build-ext-staticlib.sh`** — Xcode Run Script phase: builds the
  `ext` staticlib for the target arch(es) and drops it in `$BUILT_PRODUCTS_DIR`.
- **`compose.yaml`** — RustFS (S3-compatible) for local backend testing.

## Build & test

```bash
cargo test          # core + backend; backend runs against OpenDAL's memory service
cargo clippy --all-targets -- -D warnings
cargo fmt --all
```

`ext` builds as a `staticlib` with `cargo` (no Xcode needed to compile it). It
only becomes a *loadable* module once linked into the Xcode ExtensionKit target
and codesigned — see `xcode/README.md`. The `com.apple.developer.fskit.fsmodule`
entitlement generally needs a paid Apple Developer Program membership.

### Managing mounts (the app)

`fskit-s3-app` is a ☁ status-bar app. **Add mount…** opens a form to create a
connection — **In-memory** (the demo) or **S3** (endpoint/bucket/region/access-key
+ secret) — with *Save to Keychain*, *Mount when launching*, and *Test & Save*
(validates S3 credentials by listing the bucket). Connections persist to
`connections.json`; the menu mounts/unmounts them and auto-mounts the flagged ones
at launch.

There is **no bespoke CLI**: a connection is realised by the system `mount` tool
with its config as `-o` options, so the app and a plain `mount` do the same thing:

```bash
cargo run -p fskit-s3-app                # the app
# …or by hand (what the app runs — the extension needs an explicit `type`):
mount -F -t fskit-s3 -o type=memory ~/fskit-s3/.sources/memory ~/fskit-s3/memory
umount ~/fskit-s3/memory
```

**How the secret travels.** FSKit exposes **no credential API** — a mount gets a
resource + `FSTaskOptions` (`taskOptions` = the `-o` tokens) and nothing else. So
config rides as `-o` options, and the extension resolves the **secret** as
`Keychain[name]` (the secure default, via a shared keychain access group) **else**
an `-o secret` (insecure, visible in `ps`/`mount`). The extension is a **headless**
app extension and can't prompt, so the *app* prompts for a missing secret. The
config file never stores the secret. Sharing the Keychain item needs a signed
build + the `keychain-access-groups` entitlement on both targets (see
`xcode/README.md`).

### Adding a storage backend (e.g. WebDAV)

1. Enable the OpenDAL feature in `backend/Cargo.toml` (`services-webdav`).
2. Add a constructor next to `OpenDalBackend::s3` that builds the `Operator`.
3. Route to it from the ext's config path. The trait, `core`, and the FSKit glue
   do not change.

## Building & running the extension

The `ext` crate compiles to a Rust `staticlib` linked into an Xcode ExtensionKit
target. **It mounts and serves files today** on macOS 26 — the in-memory demo for
a `memory` connection, and a **real S3 bucket** for an S3 connection (config via
`-o`, secret from the shared Keychain group):

```sh
xcodegen generate                 # -> fskit-s3.xcodeproj (from project.yml)
open fskit-s3.xcodeproj           # pick the BBN team, Build & Run the host app
# System Settings ▸ Login Items & Extensions ▸ File System Extensions ▸ enable it
mkdir -p /tmp/fskit-s3-src /tmp/fskit-s3
mount -F -t fskit-s3 /tmp/fskit-s3-src /tmp/fskit-s3   # PathURL resource arg
ls /tmp/fskit-s3                  # -> photos/  readme.txt
cat /tmp/fskit-s3/readme.txt      # -> mounted by fskit-s3
```

Faster iteration without opening Xcode: `xcodebuild -scheme fskit-s3-host
-allowProvisioningUpdates build`, copy the `.app` to `/Applications`, then
`pluginkit -a <appex>` + `pluginkit -e use -i dev.lucsoft.fskit-s3.ext`.

Requires: full Xcode; the restricted `com.apple.developer.fskit.fsmodule`
entitlement (needs a **paid** team + the FSKit Module capability on the App ID).

### FSKit runtime gotchas (each cost hours — don't relearn them)

- **Info.plist**: the `FS*` keys go INSIDE `EXAppExtensionAttributes`, not top
  level. A *complete* module also declares `FSPersonalities`, `FSMediaTypes`, and
  `FSActivate/Check/FormatOptionSyntax`, or `fskit_agent` won't return it to
  `mount` ("No extension with fsShortName found"). Device-less FS →
  `FSSupportsPathURLs = true` (the `mount` resource arg is a path).
- **`ENABLE_DEBUG_DYLIB = NO`**: Xcode 16's stub-executor breaks system-launched
  app extensions.
- **`containerStatus`**: `loadResource` MUST set it to `ready`
  (`FSContainerStatus.ready`) or FSKit rejects with "unexpected container state"
  (POSIX 35). `unloadResource` sets it back to `notReady` so remounts start clean.
- **Singleton delegate**: the Swift `@main` reads `fileSystem` repeatedly, so
  `fskit_s3_make_filesystem` returns one cached instance (else duplicate
  containers register).
- **Stable container UUID** across probe calls (random → two containers/resource).
- **Ownership**: objects FSKit keeps past the reply (the volume from `load`,
  items from `activate`/`lookup`) must be `Retained::into_raw`'d — a borrowed
  pointer dangles and crashes the extension.
- **`enumerate`**: pack `FSItemAttributes` inline in `packEntry`, or entries
  don't show up in `ls`.
- **`-o` options need an option syntax**: to accept `mount -o key=value,…`, the
  Info.plist's `FSActivateOptionSyntax` must declare a getopt string with `o:`
  (Apple's msdos uses `u:g:m:o:`). Empty ⇒ `mount` fails with "Argument count N not
  equal to expected count 2". The extension then reads them from
  `FSTaskOptions.taskOptions`. Connection names must be shell-safe (no spaces/
  slashes) since they ride the `-o` string.
- Nuclear reset for accumulated daemon state: `sudo killall fskitd`.

Next: verify the S3 path end-to-end on a signed build (framework linking + reading
the shared Keychain group from the `fskitd` sandbox); move the app's "Test & Save"
network check off the main thread. (Connection edit + delete are done — each
connection's submenu has *Update…*, and the edit form has a red *Delete* button.)

## The Photos question (deferred)

The original motivation was hosting a **Photos** library on remote storage, which
SMB/NFS-loopback FUSE hacks can't do (Photos rejects network volumes —
`volumeIsLocal == false`). A **block-device** FSKit filesystem mounts as a
genuine *local* volume, clearing that check — but Photos has a second gate
(APFS-class capabilities: copy-on-write cloning, ownership), and whether it keys
on the literal `apfs` format or on advertised capabilities is **untested**. This
project models a *resource (unary)* filesystem, not a block-device one, so Photos
support is a separate track: spike the block-device flavor + the capability gate
before investing. Current target is the general bucket mount.

## Conventions

- Code, comments, commit messages in **English**.
- Async everywhere below the FSKit boundary; keep `core` dependency-light.
- New backend behavior gets a unit test against OpenDAL's memory service (no live
  bucket in tests/CI). Live-endpoint tests are `#[ignore]`d and opt-in via env.
- Errors cross the trait as `StorageError`; the ext is the single place that maps
  them to errno/`FSKitError`.
- **No panics in library code.** `unwrap`/`expect`/`panic!`/indexing are denied
  by clippy outside `#[cfg(test)]` (see the `deny(...)` attrs in `core`/`backend`).
  Prefer `?`, `match`, `.get(..).unwrap_or(..)`, and saturating/checked arithmetic.
- **Wrap `unsafe` in checked safe functions.** All `objc2`/FFI `unsafe` (ext,
  app) lives behind a small safe wrapper that validates arguments and
  null/again-checks results; callers never write `unsafe` directly.
- **Pin dependency features; no `default-features`.** Every dependency sets
  `default-features = false` and lists exactly the features used, each annotated
  with why. This matters most for the `objc2` crates: `default` turns on the whole
  framework, and objc2 gates each type/method behind `cfg(all(feature = "Self",
  feature = "Super", …))`, so a class also needs its full superclass chain (e.g.
  `NSStatusBarButton` → `NSButton` → `NSControl` → `NSView` → `NSResponder`) and
  any cross-cutting feature its signatures touch (`objc2-core-foundation` for
  `CGFloat`; `NSDictionary` for `NSError`'s `userInfo:`). When adding a call,
  build and let the unresolved-import/`no method` errors name the missing feature.
