use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSSecureTextField};
use objc2_foundation::{NSRect, NSString};

/// Result of a password prompt dialog.
pub struct PasswordResult {
    pub profile_id: String,
    pub password: String,
    #[allow(dead_code)]
    pub remember: bool,
}

/// Show a native macOS password prompt using NSAlert.
pub fn show_password_prompt(
    mtm: MainThreadMarker,
    profile_id: &str,
    profile_name: &str,
) -> Option<PasswordResult> {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&format!(
        "Enter password for \"{}\"",
        profile_name
    )));
    alert.setInformativeText(&NSString::from_str(
        "Your password is required to connect to the VPN.",
    ));
    alert.setAlertStyle(NSAlertStyle::Informational);
    alert.addButtonWithTitle(&NSString::from_str("Connect"));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));

    let frame = NSRect::new(
        objc2_foundation::NSPoint::new(0.0, 0.0),
        objc2_foundation::NSSize::new(300.0, 24.0),
    );
    let input = NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), frame);
    input.setPlaceholderString(Some(&NSString::from_str("Password")));
    alert.setAccessoryView(Some(&input));

    alert.setShowsSuppressionButton(true);
    if let Some(checkbox) = alert.suppressionButton() {
        checkbox.setTitle(&NSString::from_str("Remember password"));
    }

    alert.layout();
    if let Some(window) = Some(alert.window()) {
        window.makeFirstResponder(Some(&input));
    }

    let response = alert.runModal();
    if response == NSAlertFirstButtonReturn {
        let password = input.stringValue().to_string();
        let remember = alert
            .suppressionButton()
            .map(|btn| btn.state() == 1)
            .unwrap_or(false);
        Some(PasswordResult {
            profile_id: profile_id.to_string(),
            password,
            remember,
        })
    } else {
        None
    }
}
