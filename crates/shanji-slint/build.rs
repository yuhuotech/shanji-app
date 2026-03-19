fn main() {
    slint_build::compile("ui/main.slint").expect("failed to compile Slint UI");

    // Embed Info.plist into the binary on macOS so TCC can resolve the bundle ID.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist = std::path::Path::new("Info.plist");
        if plist.exists() {
            println!("cargo:rerun-if-changed=Info.plist");
            let plist_path = plist.canonicalize().expect("canonicalize Info.plist");
            println!("cargo:rustc-link-arg=-sectcreate");
            println!("cargo:rustc-link-arg=__TEXT");
            println!("cargo:rustc-link-arg=__info_plist");
            println!("cargo:rustc-link-arg={}", plist_path.display());
        }
    }
}
