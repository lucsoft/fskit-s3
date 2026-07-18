//! Dev convenience: stream the FSKit extension's unified-log output into the
//! `cargo run` terminal, so `[fskit-s3]` lines from `loadResource` (the raw
//! `taskOptions`, backend choice, failures) show up without a separate
//! `log stream`. Debug builds only — a signed release app doesn't spawn this.

/// Start tailing the extension's `[fskit-s3]` log lines to stdout, in a background
/// thread. No-op in release builds.
#[cfg(debug_assertions)]
pub fn start() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    let spawned = Command::new("/usr/bin/log")
        .args([
            "stream",
            "--style",
            "compact",
            "--info",
            // Only the extension's own process (our `[fskit-s3]` NSLog lines + FSKit's
            // in-process messages). Matching on the message text instead catches every
            // system log that merely mentions the `fskit-s3` *path*.
            "--predicate",
            "process == \"fskit-s3-ext\"",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();

    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => {
            eprintln!("[app] couldn't start extension log stream: {e}");
            return;
        }
    };
    let Some(stdout) = child.stdout.take() else {
        return;
    };
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            println!("[ext-log] {line}");
        }
        let _ = child.wait();
    });
    eprintln!("[app] streaming extension logs (debug build)");
}

/// Release builds don't tail the log.
#[cfg(not(debug_assertions))]
pub fn start() {}
