fn main() {
    // Set RH_EDGE_BASE in environment before building a release.
    // Dev builds without it will produce an empty default, causing a runtime error
    // only if the user tries to use a short code without --edge.
    let base = std::env::var("RH_EDGE_BASE").unwrap_or_default();
    println!("cargo:rustc-env=RH_EDGE_BASE={base}");
    // Re-run only when the env var changes, not on every source file touch.
    println!("cargo:rerun-if-env-changed=RH_EDGE_BASE");
}
