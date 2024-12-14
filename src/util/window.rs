use std::{
    cell::RefCell,
    marker::PhantomData,
    mem,
    pin::Pin,
    ptr::{self, NonNull},
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
    state: S,
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

impl<S> Drop for Window<S> {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.hwnd) };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowType {
    /// Visible window which receives broadcast messages from the desktop.
    TopLevel,

    /// [Message-Only Windows] are useful for windows that do not need to be
    /// visible nor need access to broadcast messages from the desktop.
    ///
    /// [Message-Only Windows]:
    /// https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features#message-only-windows
    MessageOnly,
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
    /// The `state` parameter will be allocated alongside the closure. It is
    /// meant as a convenient alternative to `Rc<State>` to access to variables
    /// from both inside and outside of the closure. A pinned reference to the
    /// state is passed as first parameter the closure. Use [`Window::state()`]
    /// to access the state from the outside.
    pub fn new<F>(
        window_type: WindowType,
        state: S,
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
            user_data: Box::into_raw(Box::new(UserData { state, wndproc })).cast(),
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
                match window_type {
                    WindowType::TopLevel => ptr::null_mut(),
                    WindowType::MessageOnly => HWND_MESSAGE,
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

    /// Same as [`Window::new()`] but allows the closure to be `FnMut`.
    ///
    /// Internally uses a `RefCell` for the closure to prevent it from being
    /// re-entered by nested message loops (e.g., from modal dialogs). Forwards
    /// nested messages to the default wndproc procedure.
    pub fn new_checked<F>(
        window_type: WindowType,
        state: S,
        wndproc: F,
    ) -> Result<Self, WindowCreationError>
    where
        F: FnMut(Pin<&S>, WindowMessage) -> Option<LRESULT> + 'static,
    {
        let wndproc = RefCell::new(wndproc);
        Self::new(window_type, state, move |state, msg| {
            // Detect when `wndproc` is re-entered, which can happen when the user
            // provided handler creates a modal dialog (e.g., a popup-menu). Rust rules
            // do not allow us to create a second mutable reference to the user-provided
            // handler. Run the default windows procedure instead.
            let mut wndproc = wndproc.try_borrow_mut().ok()?;
            wndproc(state, msg)
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
    pub fn state(&self) -> Pin<&S> {
        unsafe { Pin::new_unchecked(&self.user_data().state) }
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
    let user_data_ptr: NonNull<UserData<S, F>> = if mem::size_of::<UserData<S, F>>() == 0 {
        NonNull::dangling()
    } else {
        NonNull::new_unchecked(GetWindowLongPtrA(hwnd, GWLP_USERDATA) as _)
    };
    let user_data = user_data_ptr.as_ref();

    let ret = (user_data.wndproc)(
        Pin::new_unchecked(&user_data.state),
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
        drop(Box::from_raw(user_data_ptr.as_ptr()));
        return 0;
    }

    ret.unwrap_or_else(|| DefWindowProcA(hwnd, msg, wparam, lparam))
}

#[cfg(test)]
mod test {
    use crate::{FilterResult, MessageLoop};

    use super::*;
    use std::{
        cell::Cell,
        rc::{Rc, Weak},
    };

    #[test]
    fn create_destroy_messages() {
        let mut expected_messages = [WM_NCCREATE, WM_CREATE, WM_DESTROY, WM_NCDESTROY].into_iter();
        let mut expected_message = expected_messages.next();

        // Cannot be passed as state because we need to investigate it after drop.
        let match_cnt = Rc::new(Cell::new(0));

        let w = Window::new_checked(WindowType::TopLevel, (), {
            let match_cnt = match_cnt.clone();
            move |_, msg| {
                dbg!(msg.msg);
                if msg.msg == expected_message.unwrap() {
                    expected_message = expected_messages.next();
                    match_cnt.set(match_cnt.get() + 1);
                }
                None
            }
        })
        .unwrap();

        assert_eq!(match_cnt.get(), 2); // received WM_NCCREATE, WM_CREATE in order
        drop(w);
        assert_eq!(match_cnt.get(), 4); // received WM_DESTROY, WM_NCDESTROY in order
    }

    // Reminder for myself for why `state` cannot be mutable.
    #[test]
    fn reenter_state() {
        let state = RefCell::new(false);

        let w = Rc::new_cyclic(move |this: &Weak<Window<RefCell<bool>>>| {
            let this = this.clone();
            Window::new(WindowType::MessageOnly, state, move |state, _msg| {
                let mut state = state.borrow_mut();
                if let Some(w) = this.upgrade() {
                    // here we would get the second mutable alias
                    if w.state().try_borrow_mut().is_err() {
                        *state = true;
                        unsafe { PostMessageA(w.hwnd(), WM_USER, 0, 0) };
                    }
                }
                None
            })
            .unwrap()
        });

        // Emulate a message from a user clicking on the window somewhere.
        unsafe { PostMessageA(w.hwnd(), WM_USER, 0, 0) };
        MessageLoop::run(|msg_loop, _| {
            if *w.state().borrow() {
                msg_loop.quit();
            }
            FilterResult::Forward
        });
    }
}
