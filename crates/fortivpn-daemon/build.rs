use std::process::Command;

fn main() {
    // Build the helper binary and place it where Tauri expects sidecar binaries.
    // Use a separate target directory to avoid deadlocking on the cargo file lock
    // (the outer cargo holds the lock on target/, so nested cargo must use a different dir).
    let target = std::env::var("TARGET").unwrap();

    // On Windows, embed the manifest that requests admin elevation (for TUN + routes)
    if target.contains("windows") {
        embed_windows_manifest();
        return;
    }
    let profile = std::env::var("PROFILE").unwrap();
    let profile_flag = if profile == "release" {
        "--release"
    } else {
        "--profile=dev"
    };
    let profile_dir = if profile == "release" {
        "release"
    } else {
        "debug"
    };

    let helper_target_dir = format!(
        "{}/helper-build",
        std::env::var("OUT_DIR").unwrap_or_else(|_| "target".to_string())
    );

    let status = Command::new("cargo")
        .args([
            "build",
            "--package",
            "fortivpn-helper",
            profile_flag,
            "--target",
            &target,
            "--target-dir",
            &helper_target_dir,
        ])
        .status()
        .expect("Failed to build fortivpn-helper");

    if !status.success() {
        panic!("Failed to build fortivpn-helper");
    }

    // Copy to binaries/ with target triple suffix (Tauri sidecar convention)
    let src = format!("{helper_target_dir}/{target}/{profile_dir}/fortivpn-helper");
    let dst = format!("binaries/fortivpn-helper-{target}");

    std::fs::create_dir_all("binaries").ok();
    std::fs::copy(&src, &dst).unwrap_or_else(|e| {
        panic!("Failed to copy helper from {src} to {dst}: {e}");
    });
}

fn embed_windows_manifest() {
    println!("cargo:rerun-if-changed=daemon.manifest");

    // Write a .rc file that includes the manifest
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let rc_path = format!("{out_dir}/daemon.rc");
    let manifest_src = std::path::Path::new("daemon.manifest")
        .canonicalize()
        .unwrap();
    let manifest_escaped = manifest_src.to_str().unwrap().replace('\\', "\\\\");
    std::fs::write(&rc_path, format!("1 24 \"{manifest_escaped}\"\n")).unwrap();

    // Use the `embed_resource` approach: compile .rc to .res and link
    let res_path = format!("{out_dir}/daemon.res");
    let status = Command::new("rc.exe")
        .args(["/nologo", "/fo", &res_path, &rc_path])
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("cargo:rustc-link-arg-bin=fortivpn-daemon={res_path}");
        }
        _ => {
            // rc.exe not available (not in MSVC dev environment) — skip manifest embedding
            // The daemon will work but won't auto-request elevation
            eprintln!("Warning: rc.exe not found — daemon will not auto-request admin privileges");
            eprintln!("Run the daemon as Administrator manually for VPN connections");
        }
    }
}
