use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSSecureTextField, NSTextField,
};
use objc2_foundation::{NSRect, NSString};

use crate::app::AppState;
use crate::profile::VpnProfile;

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

// --- Settings UI using NSAlert-based dialogs ---

/// Open the settings UI. Shows a profile list dialog, then edit/create forms.
pub fn open_settings(mtm: MainThreadMarker, state: &AppState) {
    loop {
        match show_profile_list(mtm, state) {
            SettingsAction::Edit(id) => {
                show_edit_profile(mtm, state, Some(&id));
            }
            SettingsAction::New => {
                show_edit_profile(mtm, state, None);
            }
            SettingsAction::Close => break,
        }
    }
}

enum SettingsAction {
    Edit(String),
    New,
    Close,
}

/// Show the profile list as an NSAlert with informative text listing profiles.
/// Buttons: profile names + "New Profile" + "Close"
fn show_profile_list(mtm: MainThreadMarker, state: &AppState) -> SettingsAction {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str("FortiVPN Settings"));
    alert.setAlertStyle(NSAlertStyle::Informational);

    let store = state.store.lock().unwrap();
    let profiles: Vec<(String, String)> = store
        .profiles
        .iter()
        .map(|p| (p.id.clone(), p.name.clone()))
        .collect();
    drop(store);

    if profiles.is_empty() {
        alert.setInformativeText(&NSString::from_str("No profiles configured."));
    } else {
        let list: Vec<String> = {
            let store = state.store.lock().unwrap();
            store
                .profiles
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let has_pw = crate::keychain::get_password(&p.id).is_ok();
                    let pw_indicator = if has_pw { "●" } else { "○" };
                    format!(
                        "{}. {} {} ({}:{})",
                        i + 1,
                        pw_indicator,
                        p.name,
                        p.host,
                        p.port
                    )
                })
                .collect()
        };
        alert.setInformativeText(&NSString::from_str(&list.join("\n")));
    }

    // Add a button per profile for editing
    for (_, name) in &profiles {
        alert.addButtonWithTitle(&NSString::from_str(&format!("Edit {name}")));
    }
    alert.addButtonWithTitle(&NSString::from_str("New Profile"));
    alert.addButtonWithTitle(&NSString::from_str("Close"));

    let response = alert.runModal();
    let button_index = (response - 1000) as usize; // NSAlertFirstButtonReturn = 1000

    if button_index < profiles.len() {
        SettingsAction::Edit(profiles[button_index].0.clone())
    } else if button_index == profiles.len() {
        SettingsAction::New
    } else {
        SettingsAction::Close
    }
}

/// Show an edit/create profile form using NSAlert with text field accessory view.
fn show_edit_profile(mtm: MainThreadMarker, state: &AppState, profile_id: Option<&str>) {
    let existing = profile_id.and_then(|id| {
        let store = state.store.lock().unwrap();
        store.get(id).cloned()
    });

    let is_edit = existing.is_some();
    let title = if is_edit {
        "Edit Profile"
    } else {
        "New Profile"
    };

    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setAlertStyle(NSAlertStyle::Informational);

    // Build a vertical stack of labeled text fields
    let field_width: f64 = 350.0;
    let field_height: f64 = 24.0;
    let label_height: f64 = 16.0;
    let spacing: f64 = 4.0;
    let row_height = label_height + field_height + spacing;
    let fields = 5; // name, host, port, username, cert
    let pw_section_height = label_height + field_height + spacing;
    let total_height = (fields as f64 * row_height) + pw_section_height + spacing;

    let container_frame = NSRect::new(
        objc2_foundation::NSPoint::new(0.0, 0.0),
        objc2_foundation::NSSize::new(field_width, total_height),
    );
    let container =
        objc2_app_kit::NSView::initWithFrame(objc2_app_kit::NSView::alloc(mtm), container_frame);

    // Helper to create a labeled text field at a given y position
    let mut y = total_height;

    let name_val = existing.as_ref().map(|p| p.name.as_str()).unwrap_or("");
    let host_val = existing.as_ref().map(|p| p.host.as_str()).unwrap_or("");
    let port_val = existing
        .as_ref()
        .map(|p| p.port.to_string())
        .unwrap_or("443".to_string());
    let user_val = existing.as_ref().map(|p| p.username.as_str()).unwrap_or("");
    let cert_val = existing
        .as_ref()
        .map(|p| p.trusted_cert.as_str())
        .unwrap_or("");

    let name_field = add_labeled_field(mtm, &container, "Name:", &mut y, field_width, name_val);
    let host_field = add_labeled_field(mtm, &container, "Host:", &mut y, field_width, host_val);
    let port_field = add_labeled_field(mtm, &container, "Port:", &mut y, field_width, &port_val);
    let user_field = add_labeled_field(mtm, &container, "Username:", &mut y, field_width, user_val);
    let cert_field = add_labeled_field(
        mtm,
        &container,
        "Cert SHA256:",
        &mut y,
        field_width,
        cert_val,
    );

    // Password field (secure)
    let pw_label_frame = NSRect::new(
        objc2_foundation::NSPoint::new(0.0, y - label_height),
        objc2_foundation::NSSize::new(field_width, label_height),
    );
    y -= label_height;
    let pw_label = NSTextField::wrappingLabelWithString(
        &NSString::from_str("Password (leave empty to keep current):"),
        mtm,
    );
    pw_label.setFrame(pw_label_frame);
    container.addSubview(&pw_label);

    let pw_frame = NSRect::new(
        objc2_foundation::NSPoint::new(0.0, y - field_height),
        objc2_foundation::NSSize::new(field_width, field_height),
    );
    #[allow(unused_assignments)]
    {
        y -= field_height + spacing;
    }
    let pw_field = NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), pw_frame);
    pw_field.setPlaceholderString(Some(&NSString::from_str("Password")));
    container.addSubview(&pw_field);

    alert.setAccessoryView(Some(&container));

    alert.addButtonWithTitle(&NSString::from_str("Save"));
    if is_edit {
        alert.addButtonWithTitle(&NSString::from_str("Delete"));
    }
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));

    // Focus the name field
    alert.layout();
    alert.window().makeFirstResponder(Some(&name_field));

    let response = alert.runModal();
    let button_index = (response - 1000) as usize;

    if button_index == 0 {
        // Save
        let name = name_field.stringValue().to_string();
        let host = host_field.stringValue().to_string();
        let port: u16 = port_field.stringValue().to_string().parse().unwrap_or(443);
        let username = user_field.stringValue().to_string();
        let trusted_cert = cert_field.stringValue().to_string();
        let password = pw_field.stringValue().to_string();

        let id = existing
            .as_ref()
            .map(|p| p.id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let profile = VpnProfile {
            id: id.clone(),
            name,
            host,
            port,
            username,
            trusted_cert,
        };

        {
            let mut store = state.store.lock().unwrap();
            if store.get(&id).is_some() {
                store.update(profile);
            } else {
                store.add(profile);
            }
        }

        if !password.is_empty() {
            let _ = crate::keychain::store_password(&id, &password);
        }

        let _ = state.proxy.send_event(crate::app::AppEvent::RebuildTray);
    } else if is_edit && button_index == 1 {
        // Delete
        if let Some(id) = profile_id {
            let mut store = state.store.lock().unwrap();
            store.remove(id);
            let _ = crate::keychain::delete_password(id);
        }
        let _ = state.proxy.send_event(crate::app::AppEvent::RebuildTray);
    }
    // Cancel = do nothing
}

/// Add a label + text field to a container view, returning the text field.
fn add_labeled_field(
    mtm: MainThreadMarker,
    container: &objc2_app_kit::NSView,
    label_text: &str,
    y: &mut f64,
    width: f64,
    default_value: &str,
) -> objc2::rc::Retained<NSTextField> {
    let label_height: f64 = 16.0;
    let field_height: f64 = 24.0;
    let spacing: f64 = 4.0;

    let label_frame = NSRect::new(
        objc2_foundation::NSPoint::new(0.0, *y - label_height),
        objc2_foundation::NSSize::new(width, label_height),
    );
    *y -= label_height;
    let label = NSTextField::wrappingLabelWithString(&NSString::from_str(label_text), mtm);
    label.setFrame(label_frame);
    container.addSubview(&label);

    let field_frame = NSRect::new(
        objc2_foundation::NSPoint::new(0.0, *y - field_height),
        objc2_foundation::NSSize::new(width, field_height),
    );
    *y -= field_height + spacing;
    let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), field_frame);
    field.setStringValue(&NSString::from_str(default_value));
    container.addSubview(&field);

    field
}
