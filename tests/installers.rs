// Cross-file consistency checks for the installer scripts.

fn read(rel: &str) -> String {
    std::fs::read_to_string(rel).unwrap_or_else(|_| panic!("{rel} must exist"))
}

#[test]
fn installers_reference_pin_file() {
    for script in ["install.sh", "install.ps1"] {
        let text = read(script);
        assert!(
            text.contains("third-party/cloudflared.pin"),
            "{script} must fetch the pin file instead of hardcoding versions"
        );
    }
}

#[test]
fn repo_stays_static_in_installer() {
    let sh = read("install.sh");
    assert!(
        sh.contains("RH_REPO=\"DavidMANZI-093/rabbit-hole\""),
        "repo must stay a static assignment"
    );
    assert!(
        !sh.contains("${RH_REPO:-"),
        "repo must not be overridable via env"
    );
    let ps = read("install.ps1");
    assert!(
        ps.contains("$Repo = \"DavidMANZI-093/rabbit-hole\""),
        "repo must stay a static variable, not a param"
    );
    assert!(
        !ps.contains("[string]$Repo"),
        "repo must not be an overridable param"
    );
}
