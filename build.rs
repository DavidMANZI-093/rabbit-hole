fn main() {
    // Set `RH_EDGE_BASE` in environment before building a release.
    // Builds without it will produce an empty default, causing a runtime error
    // only if the user tries to use a short code without `--edge`.
    let key = "RH_EDGE_BASE";
    let base = match std::env::var(key) {
        Ok(s) => s,
        Err(e) => {
            println!("cargo:warning=could not interpret `{key}`. {e}");
            "".to_string()
        }
    };
    println!("cargo:rustc-env=RH_EDGE_BASE={base}");
    println!("cargo:rerun-if-env-changed=RH_EDGE_BASE");
}
