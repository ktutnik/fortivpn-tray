use std::process::Command;

fn main() {
    // Build the helper binary and place it where Tauri expects sidecar binaries.
    // Use a separate target directory to avoid deadlocking on the cargo file lock
    // (the outer cargo holds the lock on target/, so nested cargo must use a different dir).
    let target = std::env::var("TARGET").unwrap();
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

    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-lib=framework=IOKit");

    tauri_build::build()
}
