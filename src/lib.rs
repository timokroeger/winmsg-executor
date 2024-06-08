#![doc = include_str!("../README.md")]

mod backend;
pub mod util;

use std::{
    cell::Cell,
    future::Future,
    mem::MaybeUninit,
    pin::pin,
    ptr,
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

use windows_sys::Win32::{
    Foundation::*, System::Threading::GetCurrentThreadId, UI::WindowsAndMessaging::*,
};

/// Runs a future to completion on the calling threads message loop.
///
/// This runs the provided future on the current thread, blocking until it
/// is complete. Any tasks spawned which the future spawns internally will
/// be executed no the same thread.
///
/// Any spawned tasks will be suspended after `block_on` returns. Calling
/// `block_on` again will resume previously spawned tasks.
///
/// # Panics
///
/// Panics when the message loops is running already. This happens when
/// `block_on` or `run` is called from async tasks running on this executor.
pub fn block_on<T: 'static>(future: impl Future<Output = T> + 'static) -> T {
    // Wrap the future so it quits the message loop when finished.
    let task = backend::spawn(async move {
        let result = future.await;
        unsafe { PostQuitMessage(0) };
        result
    });
    run_message_loop();
    poll_assume_ready(task)
}

/// Runs the message loop.
///
/// Executes previously [`spawn`]ed tasks.
///
/// # Panics
///
/// Panics when the message loops is running already. This happens when
/// `block_on` or `run` is called from async tasks running on this executor.
pub fn run_message_loop() {
    thread_local!(static MESSAGE_LOOP_RUNNING: Cell<bool> = const { Cell::new(false) });
    assert!(
        !MESSAGE_LOOP_RUNNING.replace(true),
        "a message loop is running already"
    );

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

/// An owned permission to join on a task (await its termination).
///
/// If a `JoinHandle` is dropped, then its task continues running in the background
/// and its return value is lost.
pub type JoinHandle<F> = backend::JoinHandle<F>;

/// Spawns a new future on the current thread.
///
/// This function may be used to spawn tasks when the message loop is not
/// running. The provided future will start running once the message loop
/// is entered with [`MessageLoop::block_on()`] or [`MessageLoop::run()`].
pub fn spawn<F: Future + 'static>(future: F) -> JoinHandle<F> {
    backend::spawn(future)
}
