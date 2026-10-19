// UnRAR (via `unrar_sys`) calls registry, token and CryptoAPI functions but does not link
// advapi32 itself; binaries that do not pull it in otherwise fail to link.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-lib=advapi32");
    }
}
