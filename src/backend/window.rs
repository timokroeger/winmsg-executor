use std::{
    future::Future,
    sync::Arc,
    task::{Context, Wake, Waker},
};

use windows_sys::Win32::{Foundation::*, UI::WindowsAndMessaging::*};

use crate::window::create_window;

const MSG_ID_WAKE: u32 = WM_USER;

struct Task(HWND);

impl Wake for Task {
    fn wake(self: std::sync::Arc<Self>) {
        unsafe { PostMessageA(self.0, MSG_ID_WAKE, 0, 0) };
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.0) };
    }
}

pub fn dispatch(_msg: &MSG) -> bool {
    // Forward all message and let windows handle the dispatching of messages
    // to each tasks wndproc.
    false
}

pub fn spawn(future: impl Future<Output = ()> + 'static) {
    let mut future = Box::pin(future);
    let mut waker = None;

    // Create a message only window to run the taks.
    create_window(Box::new(
        move |hwnd: HWND, msg: u32, _wparam: WPARAM, _lparam: LPARAM| {
            if msg == WM_CREATE {
                waker = Some(Waker::from(Arc::new(Task(hwnd))));
            }

            if msg == WM_CREATE || msg == MSG_ID_WAKE {
                // Poll the tasks future
                if future
                    .as_mut()
                    .poll(&mut Context::from_waker(waker.as_ref().unwrap()))
                    .is_ready()
                {
                    // Remove this tasks waker reference.
                    waker = None;
                }
                Some(0)
            } else {
                None
            }
        },
    ));
}
