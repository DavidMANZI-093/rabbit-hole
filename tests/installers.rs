// Copyright (c) 2026 David MANZI
// SPDX-License-Identifier: MIT

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

#[test]
fn installers_parse() {
    for script in ["install.sh", "uninstall.sh"] {
        let Ok(status) = std::process::Command::new("bash")
            .args(["-n", script])
            .status()
        else {
            return;
        };
        assert!(status.success(), "{script} must pass `bash -n`");
    }
}

#[test]
fn unix_installer_uses_tar_gz_only() {
    let sh = read("install.sh");
    assert!(
        sh.contains(".tar.gz"),
        "install.sh must use the .tar.gz archive"
    );
    assert!(
        !sh.contains(".zip"),
        "install.sh must not fall back to the redundant .zip archive"
    );
    assert!(
        !sh.contains("need unzip"),
        "unzip is not needed for Unix releases"
    );
}

#[test]
fn installers_resolve_latest_without_the_api() {
    let latest = "https://github.com/$RH_REPO/releases/latest\"";
    let sh = read("install.sh");
    let redirect = sh
        .find(latest)
        .expect("install.sh must use the releases/latest redirect");
    let api = sh.find("https://api.github.com").unwrap_or(usize::MAX);
    assert!(
        redirect < api,
        "install.sh must try the redirect before the API"
    );
    let ps = read("install.ps1");
    assert!(
        ps.contains("https://github.com/$Repo/releases/latest\""),
        "install.ps1 must use the releases/latest redirect"
    );
}

#[test]
fn installers_bound_network_waits() {
    let sh = read("install.sh");
    assert!(
        sh.contains("--connect-timeout"),
        "curl calls need a connect timeout so an unreachable host fails fast"
    );
}

#[test]
fn release_publishes_what_installers_fetch() {
    let wf = read(".github/workflows/build.yml");
    assert!(
        wf.contains(r#"if [[ "$name" == *windows* ]]; then"#),
        "build.yml must choose the archive format per platform"
    );
    assert!(wf.contains("zip -q -X"), "Windows must publish a .zip");
    assert!(
        wf.contains(".tar.gz"),
        "Unix must publish a .tar.gz archive"
    );
    for needle in ["SHA256SUMS", "install.sh", "install.ps1"] {
        assert!(wf.contains(needle), "build.yml must publish {needle}");
    }
    assert!(
        wf.contains("Cargo.toml") && wf.contains("GITHUB_REF_NAME"),
        "build.yml must check the tag against Cargo.toml"
    );
    // asset naming must agree between the workflow and the installers
    assert!(wf.contains("name: rh-${{ github.ref_name }}-${{ matrix.target }}"));
    assert!(read("install.sh").contains("ASSET=\"rh-$RH_VERSION-$TARGET\""));
    assert!(read("install.ps1").contains("rh-$Version-$Target.zip"));
    assert!(read("install.sh").contains("SHA256SUMS"));
    assert!(read("install.ps1").contains("SHA256SUMS"));
}
