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

| Crate     | What                                                              | State                                                              |
| --------- | ---------------------------------------------------------------- | ----------------------------------------------------------------- |
| `core`    | async `StorageBackend` trait + path/key helpers + in-memory demo | ✅ complete, tested                                                |
| `backend` | `StorageBackend` over OpenDAL (`services-s3`)                     | ✅ compiles + tested against OpenDAL's memory service. No live bucket yet. |
| `ext`     | FSKit glue: `FSUnaryFileSystem`-via-`objc2` + tokio bridge        | 🚧 skeleton — needs full Xcode to build/run                       |
| `bundle`  | `.appex`/app Info.plists, entitlements, assembly Makefile         | 🚧 templates — reconcile against Xcode's FSKit target             |

Read-only for now (list + read). Target: a general-purpose bucket mount.

## Architecture

```
FSKit (fskitd) ──ObjC──> ext (objc2 FSUnaryFileSystem subclass + tokio)
                                   │  async StorageBackend trait
                                   ▼
                        core::StorageBackend  ◄── backend (OpenDAL Operator)
                                                    S3 today; WebDAV/SFTP = feature flag
```

FSKit's request vocabulary maps 1:1 onto the trait:

| FSKit volume op                     | `StorageBackend` |
| ----------------------------------- | ---------------- |
| `enumerateDirectory`                | `list`           |
| `lookupItemNamed` / `getAttributes` | `stat`           |
| `readFromFile … offset length`      | `read`           |

The trait is **async**; the ext holds a tokio runtime and fires FSKit's reply
blocks on task completion, so latency-bound network reads run concurrently. See
`CLAUDE.md` for the full design and rationale.

## Build & test (works today, no Xcode)

```bash
cargo test          # core + backend (backend tests run against OpenDAL's memory service)
```

## Building the extension (needs full Xcode)

The `ext` crate produces the Mach-O inside the `.appex`. A *loadable* module
needs full Xcode (for the FSKit "File System Extension" target template) and a
codesigning identity — see `CLAUDE.md` › Building the extension.

## License

MIT
