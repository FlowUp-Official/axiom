// Mark the crate as a release build when `AXIOM_RELEASE` is set (the GitHub
// Actions release workflow sets it). Publishing the value as `cargo:rustc-env`
// and declaring `rerun-if-env-changed` makes Cargo's fingerprinting track it,
// so cached builds can never reuse a binary built without the marker (or keep
// a release-marked binary for a development build). `config.rs` gates the
// version-pinned `$schema` URL on it.
fn main() {
    println!("cargo:rerun-if-env-changed=AXIOM_RELEASE");
    if std::env::var("AXIOM_RELEASE").is_ok() {
        println!("cargo:rustc-env=AXIOM_RELEASE=1");
    }
}
