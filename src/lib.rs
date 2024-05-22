#![doc = include_str!("../README.md")]

mod backend;
pub mod window;

use std::{
    cell::Cell,
    future::Future,
    mem::MaybeUninit,
    rc::{Rc, Weak},
};

use windows_sys::Win32::{
    Foundation::*, System::Threading::GetCurrentThreadId, UI::WindowsAndMessaging::*,
};

thread_local! {
    static MESSAGE_LOOP: Cell<Weak<QuitMessageLoopOnDrop>> = const { Cell::new(Weak::new()) };
}

// TODO: rename to something like `message_loop`
/// Runs the provided future on the current thread.
/// Waits for all spawned futures to complete before returning.
pub fn run(future: impl Future<Output = ()> + 'static) {
    // "Call PeekMessage as shown here to force the system to create the message queue."
    // https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-postthreadmessagea
    let mut msg = MaybeUninit::uninit();
    unsafe { PeekMessageA(msg.as_mut_ptr(), 0, WM_USER, WM_USER, PM_NOREMOVE) };

    // Any modal window (i.e. a right-click menu) blocks the main message loop
    // and dispatches messages internally. To keep the executor running use a
    // hook to get access to modal windows internal message loop.
    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 && backend::dispatch(&*(lparam as *const MSG)) {
            1
        } else {
            CallNextHookEx(0, code, wparam, lparam)
        }
    }
    let hook = unsafe { SetWindowsHookExA(WH_MSGFILTER, Some(hook_proc), 0, GetCurrentThreadId()) };

    // Prepare the thread local message loop reference for nested `spawn()`
    // calls inside the top-level future to work.
    let msg_loop = Rc::new(QuitMessageLoopOnDrop);
    MESSAGE_LOOP.set(Rc::downgrade(&msg_loop));
    spawn_to_loop(future, msg_loop);

    // Run the message loop.
    loop {
        let mut msg = MaybeUninit::uninit();
        unsafe {
            let ret = GetMessageA(msg.as_mut_ptr(), 0, 0, 0);
            let msg = msg.assume_init();
            match ret {
                1 => {
                    if !backend::dispatch(&msg) {
                        TranslateMessage(&msg);
                        DispatchMessageA(&msg);
                    }
                }
                0 => break,
                _ => unreachable!(),
            }
        }
    }

    unsafe { UnhookWindowsHookEx(hook) };
}

struct QuitMessageLoopOnDrop;

impl Drop for QuitMessageLoopOnDrop {
    fn drop(&mut self) {
        unsafe { PostQuitMessage(0) };
    }
}

/// Spawn a new future on the current thread.
/// Must be called from an existing task.
pub fn spawn(future: impl Future<Output = ()> + 'static) {
    // Get a strong reference to this threads message loop.
    let msg_loop = MESSAGE_LOOP.with(|msg_loop_cell| {
        let weak = msg_loop_cell.take();
        let strong = weak.upgrade().expect(
            "no message loop available: \
            `spawn()` must be called from within a future executed by `run()`",
        );
        msg_loop_cell.set(weak);
        strong
    });
    spawn_to_loop(future, msg_loop);
}

fn spawn_to_loop(future: impl Future<Output = ()> + 'static, msg_loop: Rc<QuitMessageLoopOnDrop>) {
    let future = async move {
        // Keep the message loop alive as long as the future runs.
        let _msg_loop = msg_loop;
        future.await;
    };
    backend::spawn(future);
}
