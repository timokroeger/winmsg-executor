#![doc = include_str!("../README.md")]

mod backend;
pub mod window;

use std::{
    cell::Cell,
    future::Future,
    marker::PhantomData,
    mem::MaybeUninit,
    pin::pin,
    ptr,
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

use windows_sys::Win32::{
    Foundation::*, System::Threading::GetCurrentThreadId, UI::WindowsAndMessaging::*,
};

thread_local! {
    static MESSAGE_LOOP_RUNNING: Cell<bool> = const { Cell::new(false) };
}

/// Represents this threads [windows message loop][1].
/// To execute async code within the message loop use the
/// [`block_on()`](MessageLoop::block_on) method.
///
/// [1]: https://learn.microsoft.com/en-us/windows/win32/winmsg/messages-and-message-queues
pub struct MessageLoop {
    _not_send: PhantomData<*const ()>,
}

impl MessageLoop {
    /// Creates a message queue for the current thread but does not run the
    /// dispatch loop for it yet.
    pub fn new() -> Self {
        // "Call PeekMessage as shown here to force the system to create the message queue."
        // https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-postthreadmessagea
        let mut msg = MaybeUninit::uninit();
        unsafe { PeekMessageA(msg.as_mut_ptr(), 0, WM_USER, WM_USER, PM_NOREMOVE) };
        Self {
            _not_send: PhantomData,
        }
    }

    /// Runs a future to completion on the message loop.
    ///
    /// This runs the provided future on the current thread, blocking until it
    /// is complete. Any tasks spawned which the future spawns internally will
    /// be executed no the same thread.
    ///
    /// Any spawned tasks will be suspended after `block_on` returns. Calling
    /// `block_on` again will resume previously spawned tasks.
    pub fn block_on<T: 'static>(&self, future: impl Future<Output = T> + 'static) -> T {
        MESSAGE_LOOP_RUNNING.set(true);

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
        let hook =
            unsafe { SetWindowsHookExA(WH_MSGFILTER, Some(hook_proc), 0, GetCurrentThreadId()) };

        // Wrap the future so it quits the message loop when finished.
        let task = backend::spawn(async move {
            let result = future.await;
            unsafe { PostQuitMessage(0) };
            result
        });

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
        MESSAGE_LOOP_RUNNING.set(false);

        poll_assume_ready(task)
    }
}

fn poll_assume_ready<T>(future: impl Future<Output = T>) -> T {
    const NOOP_WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
        |_| RawWaker::new(ptr::null(), &NOOP_WAKER_VTABLE),
        |_| (),
        |_| (),
        |_| (),
    );
    let noop_waker = unsafe { Waker::from_raw(RawWaker::new(ptr::null(), &NOOP_WAKER_VTABLE)) };
    let future = pin!(future);
    if let Poll::Ready(result) = future.poll(&mut Context::from_waker(&noop_waker)) {
        result
    } else {
        panic!();
    }
}

/// If a `Task` is dropped, then the task continues running in the background
/// and its return value is lost.
pub type Task<T> = backend::Task<T>;

/// Spawns a new future on the current thread.
///
/// Must be called within [`block_on()`](MessageLoop::block_on) or any
/// previously spawned task.
///
/// # Panics
///
/// Panics when no message loop is running.
pub fn spawn<T: 'static>(future: impl Future<Output = T> + 'static) -> Task<T> {
    let running = MESSAGE_LOOP_RUNNING.get();
    assert!(
        running,
        "no message loop available: \
        `spawn` must be called from within a future executed by `MessageLoop::block_on`"
    );
    backend::spawn(future)
}
