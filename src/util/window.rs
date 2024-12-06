use std::{
    cell::{Cell, RefCell},
    marker::PhantomData,
    pin::Pin,
    ptr,
    sync::Once,
};

use windows_sys::Win32::{Foundation::*, UI::WindowsAndMessaging::*};

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
    unsafe { ptr::from_ref(&__ImageBase) as _ }
}

struct SubClassInformation {
    wndproc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
    // Erased pointer type allows `wndproc_setup` to be free of generics.
    // It simply forwards the pointer and does not need to know type details.
    user_data: *const (),
}

/// Wrapper for the arguments to the [`WNDPROC callback function`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nc-winuser-wndproc).
#[derive(Debug, Clone)]
pub struct WindowMessage {
    pub hwnd: HWND,
    pub msg: u32,
    pub wparam: WPARAM,
    pub lparam: LPARAM,
}

#[repr(C)]
struct UserData<S, F> {
    ref_count: Cell<usize>,
    shared_state: S,
    wndproc: F,
}

/// Owned window handle.
///
/// Dropping the handle destroys the window.
#[derive(Debug)]
pub struct Window<S> {
    hwnd: HWND,
    _state: PhantomData<S>,
}

impl<S> Clone for Window<S> {
    fn clone(&self) -> Self {
        let ref_count = &self.user_data().ref_count;
        ref_count.set(ref_count.get() + 1);
        Self {
            hwnd: self.hwnd,
            _state: self._state,
        }
    }
}

unsafe impl<S: Send> Send for Window<S> {}
unsafe impl<S: Sync> Sync for Window<S> {}

impl<S> Drop for Window<S> {
    fn drop(&mut self) {
        let ref_count = &self.user_data().ref_count;
        ref_count.set(ref_count.get() - 1);
        if ref_count.get() == 0 {
            unsafe { DestroyWindow(self.hwnd) };
        }
    }
}

/// Window could not be created.
///
/// Possible failure reasons:
/// * `WM_NCCREATE` message was handled but did not return 0
/// * `WM_CREATE` message was handled but returned -1
/// * Reached the maximum number of 10000 window handles per process:
///   <https://devblogs.microsoft.com/oldnewthing/20070718-00/?p=25963>
#[derive(Debug)]
pub struct WindowCreationError;

impl<S> Window<S> {
    /// Creates a new window with a `wndproc` closure.
    ///
    /// The `shared_state` parameter will be allocated alongside the closure.
    /// Allows for convenient access to variables from both inside and outside
    /// of the closure without an extra `Rc<State>`. Use the
    /// [`Window::shared_state()`] method to access the state from the outside.
    ///
    /// [Message-Only Windows] are useful for windows that do not need to be
    /// visible nor need access to broadcast messages from the desktop.
    ///
    /// [Message-Only Windows]:
    /// https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features#message-only-windows
    ///
    /// Internally uses a `RefCell` for the closure to prevent it from being
    /// re-entered by nested message loops (e.g., from modal dialogs). Forwards
    /// nested messages to the default wndproc procedure. If you require more
    /// control for those scenarios, use [`Window::new_reentrant()`].
    pub fn new<F>(
        message_only: bool,
        shared_state: S,
        wndproc: F,
    ) -> Result<Self, WindowCreationError>
    where
        F: FnMut(Pin<&S>, WindowMessage) -> Option<LRESULT> + 'static,
    {
        let wndproc = RefCell::new(wndproc);
        Self::new_reentrant(message_only, shared_state, move |state, msg| {
            // Detect when `wndproc` is re-entered, which can happen when the user
            // provided handler creates a modal dialog (e.g., a popup-menu). Rust rules
            // do not allow us to create a second mutable reference to the user-provided
            // handler. Run the default windows procedure instead.
            let mut wndproc = wndproc.try_borrow_mut().ok()?;
            wndproc(state, msg)
        })
    }

    /// Same as [`Window::new()`] but allows the closure to be re-entered.
    pub fn new_reentrant<F>(
        message_only: bool,
        shared_state: S,
        wndproc: F,
    ) -> Result<Self, WindowCreationError>
    where
        F: Fn(Pin<&S>, WindowMessage) -> Option<LRESULT> + 'static,
    {
        let class_name = c"winmsg-executor".as_ptr().cast();

        // A class must only be unregistered when it was registered from a DLL which
        // is unloaded during program execution: For now, an unsupported use case.
        static CLASS_REGISTRATION: Once = Once::new();
        CLASS_REGISTRATION.call_once(|| {
            let mut wnd_class: WNDCLASSA = unsafe { std::mem::zeroed() };
            wnd_class.lpfnWndProc = Some(wndproc_setup);
            wnd_class.hInstance = get_instance_handle();
            wnd_class.lpszClassName = class_name;
            unsafe { RegisterClassA(&wnd_class) };
        });

        // Pass the closure and state as user data to our typed window process.
        let subclassinfo = SubClassInformation {
            wndproc: wndproc_typed::<S, F>,
            user_data: Box::into_raw(Box::new(UserData {
                ref_count: Cell::new(1),
                shared_state,
                wndproc,
            }))
            .cast(),
        };

        let hwnd = unsafe {
            CreateWindowExA(
                0,
                class_name,
                ptr::null(),
                0,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                if message_only {
                    HWND_MESSAGE
                } else {
                    ptr::null_mut()
                },
                ptr::null_mut(),
                get_instance_handle(),
                // The subclass info can be passed as a pointer to the stack
                // allocated variable because it will only be accessed during
                // the `CreateWindowExA()` call and not afterward.
                ptr::from_ref(&subclassinfo).cast(),
            )
        };
        if hwnd.is_null() {
            return Err(WindowCreationError);
        }

        Ok(Self {
            hwnd,
            _state: PhantomData,
        })
    }

    fn user_data(&self) -> &UserData<S, ()> {
        unsafe { &*(GetWindowLongPtrA(self.hwnd, GWLP_USERDATA) as *const _) }
    }

    /// Returns this windows raw window handle.
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Returns a reference to the state shared with the `wndproc` closure.
    pub fn shared_state(&self) -> Pin<&S> {
        unsafe { Pin::new_unchecked(&self.user_data().shared_state) }
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
        SetWindowLongPtrA(hwnd, GWLP_WNDPROC, subclassinfo.wndproc as usize as _);

        // Attach user data to the window so it can be accessed from the
        // `wndproc` callback function when receiving other messages.
        // https://devblogs.microsoft.com/oldnewthing/20191014-00/?p=102992
        SetWindowLongPtrA(hwnd, GWLP_USERDATA, subclassinfo.user_data as _);

        // Forward this message to the freshly registered subclass wndproc.
        SendMessageA(hwnd, msg, wparam, lparam)
    } else {
        // This code path is only reached for messages before `WM_NCCREATE`.
        // On Windows 10/11 `WM_GETMINMAXINFO` is the first and only message
        // before `WM_NCCREATE`.
        DefWindowProcA(hwnd, msg, wparam, lparam)
    }
}

unsafe extern "system" fn wndproc_typed<S, F>(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT
where
    F: Fn(Pin<&S>, WindowMessage) -> Option<LRESULT> + 'static,
{
    let user_data = &mut *(GetWindowLongPtrA(hwnd, GWLP_USERDATA) as *mut UserData<S, F>);

    let ret = (user_data.wndproc)(
        Pin::new_unchecked(&user_data.shared_state),
        WindowMessage {
            hwnd,
            msg,
            wparam,
            lparam,
        },
    );

    if msg == WM_CLOSE {
        // We manage the window lifetime ourselves. Prevent the default
        // handler from calling `DestroyWindow()` to keep the state
        // allocated until the window wrapper struct is dropped.
        return 0;
    }

    if msg == WM_NCDESTROY {
        // This is the very last message received by this function before
        // the window is destroyed. Deallocate the window user data.
        assert_eq!(
            user_data.ref_count.get(),
            0,
            "Received unexpected `WM_DESTROY` message. Window struct must be \
            dropped instead of calling `DestroyWindow()` directly."
        );
        drop(Box::from_raw(user_data));
        return 0;
    }

    ret.unwrap_or_else(|| DefWindowProcA(hwnd, msg, wparam, lparam))
}
