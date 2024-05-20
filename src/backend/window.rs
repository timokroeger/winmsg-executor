use std::{
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context, RawWaker, RawWakerVTable, Waker},
};

use windows_sys::Win32::{Foundation::*, UI::WindowsAndMessaging::*};

use crate::{
    window::{create_window, WindowContext},
    QuitMessageLoopOnDrop,
};

const MSG_ID_WAKE: u32 = WM_NULL;

struct TaskState {
    future: Pin<Box<dyn Future<Output = ()>>>,
    _msg_loop: Rc<QuitMessageLoopOnDrop>,
}

impl WindowContext for TaskState {
    fn wndproc(
        &mut self,
        hwnd: HWND,
        msg: u32,
        _wparam: WPARAM,
        _lparam: LPARAM,
    ) -> Option<LRESULT> {
        if msg == MSG_ID_WAKE {
            // Poll the tasks future
            if self
                .future
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
    }
}

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

pub fn spawn(msg_loop: Rc<QuitMessageLoopOnDrop>, future: impl Future<Output = ()> + 'static) {
    let state = TaskState {
        future: Box::pin(future),
        _msg_loop: msg_loop.clone(),
    };

    // Create a message only window to run the taks.
    let hwnd = create_window(state);
    debug_assert_ne!(hwnd, 0);

    // Trigger initial poll
    waker_for_window(hwnd).wake();
}
