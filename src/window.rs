use std::{ffi::CStr, ptr};

use windows_sys::Win32::{Foundation::*, UI::WindowsAndMessaging::*};

const CLASS_NAME: &CStr = c"winmsg-executor";

type WndProc = Box<dyn FnMut(HWND, u32, WPARAM, LPARAM) -> Option<LRESULT>>;

fn wndproc_from_hwnd(hwnd: HWND) -> &'static mut WndProc {
    unsafe {
        let cx_ptr = GetWindowLongPtrA(hwnd, GWLP_USERDATA) as *mut WndProc;
        cx_ptr.as_mut().unwrap()
    }
}

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

fn register_class() {
    let mut wnd_class: WNDCLASSA = unsafe { std::mem::zeroed() };
    wnd_class.lpfnWndProc = Some(wndproc);
    wnd_class.hInstance = get_instance_handle();
    wnd_class.lpszClassName = CLASS_NAME.as_ptr().cast();
    unsafe { RegisterClassA(&wnd_class) };
}

/// The window must be destroyed with a call to `DestroyWindow()` manually,
/// either using the returned handle or from within [`WindowContext::wndproc()`].
pub fn create_window(wndproc: WndProc) -> HWND {
    // When creating multiple windows the `register_class()` call only succeeds
    // the first time, after which the class exists and can be reused.
    // This means class registration API behaves like a `OnceLock`.
    // A class must only be unregistered when it was registered from a DLL which
    // is unloaded during program execution: For now an unsupported use case.
    register_class();

    unsafe {
        CreateWindowExA(
            0,
            CLASS_NAME.as_ptr().cast(),
            ptr::null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            0,
            get_instance_handle(),
            Box::into_raw(Box::new(wndproc)).cast(),
        )
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_GETMINMAXINFO {
        // This is the very first message received by this function when calling
        // `CreateWindowExA()`. The user data pointer has not been set yet.
        // Run the default handler and return early.
        return DefWindowProcA(hwnd, msg, wparam, lparam);
    }

    if msg == WM_NCCREATE {
        // Attach user data to the window so it can be accessed from this
        // callback function when receiving other messages.
        // This must be done here because the WM_NCCREATE (which is the second
        // message after `WM_GETMINMAXINFO`) and other message (e.g WM_CREATE)
        // are dispatched to this callback before `CreateWindowEx()` returns.
        // https://devblogs.microsoft.com/oldnewthing/20191014-00/?p=102992
        let create_params = lparam as *const CREATESTRUCTA;
        SetWindowLongPtrA(
            hwnd,
            GWLP_USERDATA,
            (*create_params).lpCreateParams as isize,
        );
        return 1; // Continue with window creation
    }

    let wndproc = wndproc_from_hwnd(hwnd);
    if msg == WM_NCDESTROY {
        // This is the very last message received by this function before
        // the windows is destroyed. Deallocate the window context.
        drop(Box::from_raw(wndproc));
        return 0;
    }

    wndproc(hwnd, msg, wparam, lparam).unwrap_or_else(|| DefWindowProcA(hwnd, msg, wparam, lparam))
}
