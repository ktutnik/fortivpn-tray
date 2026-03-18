//! macOS App Nap management via IOKit power assertions.
//!
//! When the VPN is disconnected, allow macOS to nap the app (reduces energy to near-zero).
//! When connected, create a power assertion to keep the app responsive.

#[cfg(target_os = "macos")]
mod inner {
    use std::ffi::c_void;
    use std::sync::Mutex;

    extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: *const c_void,
            level: u32,
            reason: *const c_void,
            assertion_id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(assertion_id: u32) -> i32;
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            c_str: *const i8,
            encoding: u32,
        ) -> *const c_void;
        fn CFRelease(cf: *const c_void);
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;

    static ASSERTION_ID: Mutex<Option<u32>> = Mutex::new(None);

    pub fn disable_app_nap() {
        let mut guard = ASSERTION_ID.lock().unwrap();
        if guard.is_some() {
            return;
        }

        unsafe {
            let assertion_type = CFStringCreateWithCString(
                std::ptr::null(),
                c"PreventUserIdleSystemSleep".as_ptr(),
                K_CF_STRING_ENCODING_UTF8,
            );
            let reason = CFStringCreateWithCString(
                std::ptr::null(),
                c"VPN tunnel active".as_ptr(),
                K_CF_STRING_ENCODING_UTF8,
            );

            let mut assertion_id: u32 = 0;
            let ret = IOPMAssertionCreateWithName(
                assertion_type,
                K_IOPM_ASSERTION_LEVEL_ON,
                reason,
                &mut assertion_id,
            );

            CFRelease(assertion_type);
            CFRelease(reason);

            if ret == 0 {
                *guard = Some(assertion_id);
            }
        }
    }

    pub fn enable_app_nap() {
        let mut guard = ASSERTION_ID.lock().unwrap();
        if let Some(id) = guard.take() {
            unsafe {
                IOPMAssertionRelease(id);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod inner {
    pub fn disable_app_nap() {}
    pub fn enable_app_nap() {}
}

pub use inner::{disable_app_nap, enable_app_nap};
