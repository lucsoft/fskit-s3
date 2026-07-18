# fskit-s3

Mount an S3 bucket (or any object store) as a native macOS volume, using
Apple's **FSKit** — a userspace filesystem, **no kernel extension, no security
downgrade**. Written in Rust; FSKit is driven directly from Rust via `objc2`
(FSKit ships plain Objective-C headers), the same way the sibling `wayland-macos`
project drives AppKit.

The name says S3, but the design is backend-agnostic: FSKit is mapped onto one
small trait (`StorageBackend`), and S3 is just the first implementor. WebDAV/SSH
can be added later as sibling crates without touching the FSKit side.

## Status

| Crate | What | State |
|-------|------|-------|
| `core` | `StorageBackend` trait + path/key helpers + `InMemoryBackend` | ✅ complete, tested (`cargo test -p fskit-s3-core`) |
| `backend-s3` | S3/S3-compatible backend: `ListObjectsV2` + ranged `GetObject`, in-crate SigV4 | ✅ compiles + tested; SigV4 pinned to RFC 4231 + AWS vectors. **Not yet exercised against a live bucket.** |
| `ext` | FSKit glue: `FSUnaryFileSystem` subclass delegating to a `StorageBackend` | 🚧 skeleton — needs full Xcode to build/run (see below) |
| `bundle` | `.appex`/app Info.plists, entitlements, assembly Makefile | 🚧 templates — reconcile against Xcode's FSKit target |

Read-only for now (list + read). Write support is a later step.

## Architecture

```
FSKit (fskitd)  ──ObjC──>  ext (objc2 FSUnaryFileSystem subclass)
                                     │  StorageBackend trait
                                     ▼
                        core::StorageBackend  ◄── backend-s3 (S3 + SigV4)
                                                   (later: WebDAV, SSH, …)
```

FSKit's request vocabulary maps 1:1 onto the trait:

| FSKit volume op | `StorageBackend` |
|-----------------|------------------|
| `enumerateDirectory` | `list` |
| `lookupItemNamed` / `getAttributes` | `stat` |
| `readFromFile … offset length` | `read` |

The trait is **blocking**, which matches both FSKit's reply-block model and S3's
request/response shape — no async runtime is pulled into the extension.

Object stores have no real directories, so a "directory" is any shared key
prefix; `core` synthesizes directory entries from prefixes (`delimiter=/` on the
S3 side) and both the in-memory and S3 backends agree on that model.

## Build & test (works today, no Xcode)

```bash
cargo test -p fskit-s3-core        # the seam
cargo test -p fskit-s3-backend-s3  # S3 client + SigV4 vectors
```

## Building the extension (needs full Xcode)

The `ext` crate produces the Mach-O that lives inside the `.appex`. Getting a
*loadable* module requires:

1. **Full Xcode** (App Store) — this machine currently has only Command Line
   Tools (`xcodebuild` absent). FSKit's extension-point plumbing and a valid
   `Info.plist`/entitlements template come from Xcode's "File System Extension"
   target; `bundle/*.plist` are best-effort templates to reconcile against it.
2. A **codesigning identity** — `fskitd` refuses to load an improperly signed
   module. Ad-hoc signing may work locally for development.

Then finish the objc2 bindings in `ext/src/lib.rs` (the class/protocol sketch is
there) and assemble:

```bash
make -f bundle/Makefile SIGN_ID="-"   # ad-hoc; or a Developer ID
```

The module then appears in **System Settings ▸ General ▸ Login Items &
Extensions ▸ File System Extensions** and mounts via `mount -F -t fskit-s3 …`.

## The Photos question

The original motivation was hosting a **Photos** library on remote storage —
which the SMB/NFS-loopback FUSE hacks can't do, because Photos rejects network
volumes outright (`volumeIsLocal == false`). A block-device FSKit filesystem
mounts as a genuine *local* volume, clearing that specific check. Photos then has
a **second** gate — it wants APFS-class capabilities (copy-on-write cloning,
ownership). Whether Photos keys on the literal `apfs` format string or on
advertised capabilities is **untested** and is the real risk to spike before
building this out for Photos specifically. Note: this crate models a *resource*
(unary) filesystem, not a block-device one; Photos support would likely require
the block-device FSKit flavor instead.

## License

MIT
