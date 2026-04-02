pub fn show(title: &str, body: &str) {
    // macOS: use osascript (notify-rust shows "Choose Application" dialog for non-bundled binaries)
    #[cfg(target_os = "macos")]
    {
        let safe_title = title.replace('"', "\\\"");
        let safe_body = body.replace('"', "\\\"");
        let script = format!("display notification \"{safe_body}\" with title \"{safe_title}\"");
        let _ = std::process::Command::new("/usr/bin/osascript")
            .args(["-e", &script])
            .spawn();
    }

    // Linux/Windows: notify-rust works fine
    #[cfg(not(target_os = "macos"))]
    {
        let _ = notify_rust::Notification::new()
            .summary(title)
            .body(body)
            .show();
    }
}
