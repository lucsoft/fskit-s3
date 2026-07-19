//! Link the system frameworks the app calls into directly, for the standalone
//! `cargo run` binary. (The Xcode `fskit-s3-host` target links the app as a
//! staticlib and adds these frameworks itself — see project.yml — so these
//! directives only matter for the dev binary's own link step.)

fn main() {
    // FSKit: the `FSClient` extension-health query (health.rs).
    println!("cargo:rustc-link-lib=framework=FSKit");
    // ServiceManagement: `SMAppService` launch-at-login registration (autostart.rs).
    println!("cargo:rustc-link-lib=framework=ServiceManagement");
}
