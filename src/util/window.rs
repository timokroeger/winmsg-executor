//! Window wrapper which allows to move state into the `wndproc` closure.

use std::{ffi::CStr, ptr};

use windows_sys::Win32::{Foundation::*, UI::WindowsAndMessaging::*};

const CLASS_NAME: &CStr = c"winmsg-executor";
// Taken from:
// https://github.com/rust-windowing/winit/blob/v0.30.0/src/platform_impl/windows/util.rs#L140
fn get_instance_handle() -> HINSTANCE {
    // Gets the instance handle by taking the address of the
    // pseudo-variable created by the microsoft linker:
    // https://devblogs.microsoft.com/oldnewthing/20041025-00/?p=37483

    // This is preferred over GetModuleHandle(NULL) because it also works in DLLs:
    // https://stackoverflow.com/questions/21718027/getmodulehandlenull-vs-hinstance

    extern "C" {
        static __ImageBase: u8;
    }
    unsafe { &__ImageBase as *const _ as _ }
}

struct SubClassInformation {
    wndproc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
    user_data: *const (),
}

/// The window must be destroyed with a call to `DestroyWindow()` manually,
/// either using the returned handle or from within the `f` closure.
pub fn create_window<T>(message_only: bool, f: T) -> HWND
where
    T: FnMut(HWND, u32, WPARAM, LPARAM) -> Option<LRESULT> + 'static,
{
    // When creating multiple windows the `RegisterClassA()` call only succeeds
    // the first time, after which the class exists and can be reused.
    // This means class registration API behaves like a `OnceLock`.
    // A class must only be unregistered when it was registered from a DLL which
    // is unloaded during program execution: For now an unsupported use case.
    let mut wnd_class: WNDCLASSA = unsafe { std::mem::zeroed() };
    wnd_class.lpfnWndProc = Some(wndproc_setup);
    wnd_class.hInstance = get_instance_handle();
    wnd_class.lpszClassName = CLASS_NAME.as_ptr().cast();
    unsafe { RegisterClassA(&wnd_class) };

    // Pass the closure as user data to our typed window process which.
    let subclassinfo = SubClassInformation {
        wndproc: wndproc::<T>,
        user_data: Box::into_raw(Box::new(f)).cast(),
    };

    unsafe {
        CreateWindowExA(
            0,
            CLASS_NAME.as_ptr().cast(),
            ptr::null(),
            0,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            if message_only { HWND_MESSAGE } else { 0 },
            0,
            get_instance_handle(),
            (&subclassinfo as *const SubClassInformation).cast(),
        )
    }
}

unsafe extern "system" fn wndproc_setup(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        let create_params = lparam as *const CREATESTRUCTA;
        let subclassinfo = &*((*create_params).lpCreateParams as *const SubClassInformation);

        // Replace our `wndproc` with the one using the correct type.
        SetWindowLongPtrA(hwnd, GWLP_WNDPROC, subclassinfo.wndproc as isize);

        // Attach user data to the window so it can be accessed from this
        // callback function when receiving other messages.
        // This must be done here because the WM_NCCREATE (which is the second
        // message after `WM_GETMINMAXINFO`) and other message (e.g WM_CREATE)
        // are dispatched to this callback before `CreateWindowEx()` returns.
        // https://devblogs.microsoft.com/oldnewthing/20191014-00/?p=102992
        SetWindowLongPtrA(hwnd, GWLP_USERDATA, subclassinfo.user_data as isize);

        1 // Continue with window creation
    } else {
        DefWindowProcA(hwnd, msg, wparam, lparam)
    }
}

unsafe extern "system" fn wndproc<T>(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT
where
    T: FnMut(HWND, u32, WPARAM, LPARAM) -> Option<LRESULT> + 'static,
{
    let wndproc = GetWindowLongPtrA(hwnd, GWLP_USERDATA) as *mut T;
    if msg == WM_NCDESTROY {
        // This is the very last message received by this function before
        // the windows is destroyed. Deallocate the window context.
        drop(Box::from_raw(wndproc));
        0
    } else {
        // Call this windows closure.
        (*wndproc)(hwnd, msg, wparam, lparam)
            .unwrap_or_else(|| DefWindowProcA(hwnd, msg, wparam, lparam))
    }
}
