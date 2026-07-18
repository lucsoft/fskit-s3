# fskit-s3

Mount an S3 bucket (or any object store) as a native macOS volume, using Apple's
**FSKit** — a userspace filesystem, **no kernel extension, no security
downgrade**. Written in Rust; FSKit is driven directly from Rust via `objc2`
(FSKit ships plain Objective-C headers), the same way the sibling `wayland-macos`
project drives AppKit.

The name says S3, but the design is backend-agnostic: FSKit is mapped onto one
small async trait (`StorageBackend`), implemented once over
[Apache OpenDAL](https://opendal.apache.org). S3 is the first service enabled;
WebDAV, SFTP, and ~40 others are a feature flag away.

## Status

- **`core`** — async `StorageBackend` trait + path/key helpers + in-memory demo.
  ✅ Complete and tested.
- **`backend`** — `StorageBackend` over OpenDAL (`services-s3`).
  ✅ Compiles and tested against OpenDAL's in-memory service. No live bucket wired
  into CI (there's an ignored test for that — see below).
- **`ext`** — FSKit glue: an `objc2` `FSUnaryFileSystem` subclass + tokio bridge.
  🚧 Skeleton; needs full Xcode to build and run.
- **`bundle`** — `.appex`/app `Info.plist`, entitlements, assembly `Makefile`.
  🚧 Templates to reconcile against Xcode's FSKit target.

Read-only for now (list + read). Target: a general-purpose bucket mount.

## Architecture

```mermaid
flowchart TD
    apps["Finder / Photos / any app"] -->|POSIX VFS| fskitd["fskitd (FSKit)"]
    fskitd -->|Objective-C| ext["ext&nbsp;— objc2 FSUnaryFileSystem subclass + tokio"]
    ext -->|"async StorageBackend trait"| core["core::StorageBackend"]
    backend["backend — OpenDAL Operator"] -.implements.-> core
    backend --> s3[("S3")]
    backend -.->|feature flag| more[("WebDAV / SFTP / …")]
```

FSKit's request vocabulary maps 1:1 onto the trait:

- `enumerateDirectory` → `list`
- `lookupItemNamed` / `getAttributes` → `stat`
- `readFromFile … offset length` → `read`

The trait is **async**; the ext holds a tokio runtime and fires FSKit's reply
blocks on task completion, so latency-bound network reads run concurrently. See
[`CLAUDE.md`](CLAUDE.md) for the full design and rationale.

## Build & test (works today, no Xcode)

```sh
cargo test          # core + backend, against OpenDAL's in-memory service
```

### Testing against a real S3 endpoint (RustFS)

```sh
docker compose up -d                                                  # local S3 on :9000
RUSTFS_ENDPOINT=http://localhost:9000 cargo test -p fskit-s3-backend -- --ignored
docker compose down                                                  # add -v to wipe data
```

## Building the extension (needs full Xcode)

The `ext` crate produces the Mach-O inside the `.appex`. A loadable module needs
full Xcode (for the FSKit "File System Extension" target template) and a
codesigning identity — see [`CLAUDE.md`](CLAUDE.md) › _Building the extension_.

## License

MIT
