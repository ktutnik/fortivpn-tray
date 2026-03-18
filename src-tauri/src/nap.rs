//! macOS App Nap management.
//!
//! When the VPN is disconnected, allow macOS to nap the app (reduces energy to near-zero).
//! When connected, disable App Nap so the tunnel stays responsive.

#[cfg(target_os = "macos")]
mod inner {
    use std::ffi::c_void;
    use std::sync::Mutex;

    // NSActivityLatencyCritical | NSActivityUserInitiated
    const ACTIVITY_OPTIONS: u64 = 0x00FF_0001;

    extern "C" {
        fn objc_getClass(name: *const i8) -> *mut c_void;
        fn sel_registerName(name: *const i8) -> *mut c_void;
        fn objc_msgSend(obj: *mut c_void, sel: *mut c_void, ...) -> *mut c_void;
        fn CFRetain(cf: *mut c_void) -> *mut c_void;
        fn CFRelease(cf: *mut c_void);
    }

    struct SendPtr(*mut c_void);
    unsafe impl Send for SendPtr {}
    unsafe impl Sync for SendPtr {}

    static ACTIVITY_TOKEN: Mutex<Option<SendPtr>> = Mutex::new(None);

    pub fn disable_app_nap() {
        let mut guard = ACTIVITY_TOKEN.lock().unwrap();
        if guard.is_some() {
            return;
        }

        unsafe {
            let process_info_cls = objc_getClass(c"NSProcessInfo".as_ptr());
            let process_info_sel = sel_registerName(c"processInfo".as_ptr());
            let info = objc_msgSend(process_info_cls, process_info_sel);
            if info.is_null() {
                return;
            }

            let nsstring_cls = objc_getClass(c"NSString".as_ptr());
            let string_sel = sel_registerName(c"stringWithUTF8String:".as_ptr());
            let reason_cstr = c"VPN tunnel active".as_ptr();
            let reason = objc_msgSend(nsstring_cls, string_sel, reason_cstr);

            let begin_sel = sel_registerName(c"beginActivityWithOptions:reason:".as_ptr());
            let token = objc_msgSend(info, begin_sel, ACTIVITY_OPTIONS, reason);
            if !token.is_null() {
                CFRetain(token);
                *guard = Some(SendPtr(token));
            }
        }
    }

    pub fn enable_app_nap() {
        let mut guard = ACTIVITY_TOKEN.lock().unwrap();
        if let Some(SendPtr(token)) = guard.take() {
            unsafe {
                let process_info_cls = objc_getClass(c"NSProcessInfo".as_ptr());
                let process_info_sel = sel_registerName(c"processInfo".as_ptr());
                let info = objc_msgSend(process_info_cls, process_info_sel);
                if !info.is_null() {
                    let end_sel = sel_registerName(c"endActivity:".as_ptr());
                    objc_msgSend(info, end_sel, token);
                }
                CFRelease(token);
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
