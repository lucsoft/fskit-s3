// Link the FSKit framework (ObjC) so the extension binary can subclass
// FSUnaryFileSystem and message the volume/item classes. Foundation comes in via
// objc2-foundation. FSKit lives in the CommandLineTools/Xcode SDK.
fn main() {
    println!("cargo:rustc-link-lib=framework=FSKit");
    println!("cargo:rustc-link-lib=framework=Foundation");
}
