/// Send a desktop notification.
pub fn send_notification(title: &str, body: &str) {
    let _ = notify_rust::Notification::new()
        .summary(title)
        .body(body)
        .show();
}

/// Post a macOS distributed notification so the Swift UI app can react instantly.
/// Used to signal VPN state changes (connected, disconnected, error) without polling.
#[cfg(target_os = "macos")]
pub fn post_distributed_notification(name: &str) {
    use std::ffi::c_void;

    extern "C" {
        fn CFNotificationCenterGetDistributedCenter() -> *mut c_void;
        fn CFNotificationCenterPostNotification(
            center: *mut c_void,
            name: *const c_void,
            object: *const c_void,
            user_info: *const c_void,
            deliver_immediately: u8,
        );
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            c_str: *const i8,
            encoding: u32,
        ) -> *const c_void;
        fn CFRelease(cf: *const c_void);
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    unsafe {
        let center = CFNotificationCenterGetDistributedCenter();
        let c_name = std::ffi::CString::new(name).unwrap();
        let cf_name = CFStringCreateWithCString(
            std::ptr::null(),
            c_name.as_ptr(),
            K_CF_STRING_ENCODING_UTF8,
        );
        CFNotificationCenterPostNotification(
            center,
            cf_name,
            std::ptr::null(),
            std::ptr::null(),
            1, // deliver immediately
        );
        CFRelease(cf_name);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn post_distributed_notification(_name: &str) {
    // No-op on non-macOS
}
