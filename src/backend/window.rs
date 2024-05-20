use std::{
    future::Future,
    task::{Context, RawWaker, RawWakerVTable, Waker},
};

use windows_sys::Win32::{Foundation::*, UI::WindowsAndMessaging::*};

use crate::window::create_window;

const MSG_ID_WAKE: u32 = WM_NULL;

fn waker_for_window(hwnd: HWND) -> Waker {
    unsafe fn wake(hwnd: *const ()) {
        PostMessageA(hwnd as HWND, MSG_ID_WAKE, 0, 0);
    }
    static VTABLE: RawWakerVTable =
        RawWakerVTable::new(|p| RawWaker::new(p, &VTABLE), wake, wake, |_| ());
    unsafe { Waker::from_raw(RawWaker::new(hwnd as *const (), &VTABLE)) }
}

pub fn dispatch(_msg: &MSG) -> bool {
    // Forward all message and let windows handle the dispatching of messages
    // to each tasks wndproc.
    false
}

pub fn spawn(future: impl Future<Output = ()> + 'static) {
    let mut future = Box::pin(future);

    // Create a message only window to run the taks.
    let hwnd = create_window(Box::new(
        move |hwnd: HWND, msg: u32, _wparam: WPARAM, _lparam: LPARAM| {
            if msg == MSG_ID_WAKE {
                // Poll the tasks future
                if future
                    .as_mut()
                    .poll(&mut Context::from_waker(&waker_for_window(hwnd)))
                    .is_ready()
                {
                    unsafe { DestroyWindow(hwnd) };
                }
                Some(0)
            } else {
                None
            }
        },
    ));
    debug_assert_ne!(hwnd, 0);

    // Trigger initial poll
    waker_for_window(hwnd).wake();
}
