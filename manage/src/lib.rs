//! Host-side management of fskit-s3 **connections** and **mounts**.
//!
//! This crate is the management logic behind the `fskit-s3-menubar` app: the
//! model of what can be mounted and the code that realises it. It answers two
//! questions:
//!
//! - *What can I mount?* → [`Connection`]s, held in an in-memory [`Registry`].
//! - *What is mounted, and how do I (un)mount it?* → [`Mount`], [`mount`],
//!   [`unmount`], [`list_fskit`].
//!
//! A **connection** is a configured storage endpoint (an S3 bucket, eventually);
//! a **mount** is a live realisation of one at a path. There is **no bespoke CLI**
//! — the system `mount`/`umount` tools already are that — so mounting a connection
//! is just running `mount` with the connection's
//! [`mount_options`](Connection::mount_options); the app builds that command, and
//! a human could type it. Today the only backend the extension serves is the
//! credential-free in-memory demo, so a connection carries just an identity, the
//! registry is seeded with a single [`Connection::demo`], and it is **not yet
//! persisted** — a fresh process starts from [`Registry::with_defaults`].
//! Persistence (a config file plus Keychain-backed secrets) is the next milestone;
//! [`ConnectionKind`] and the registry's mutation API already model it so the app
//! won't change shape when it lands.
//!
//! The crate is pure Rust with no `objc2`/AppKit dependency — it drives the
//! system `mount`/`diskutil` tools — which keeps it fully unit-testable and its
//! logic separate from the app's `unsafe` AppKit layer.

// Management code must never panic: no unwrap/expect/panic/indexing outside tests.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::unreachable
    )
)]

mod connection;
mod mount;

pub use connection::{Connection, ConnectionKind, Registry};
pub use mount::{list, list_fskit, mount, parse, unmount, Mount, FS_TYPE};
