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

use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::util::MsgFilterHook;

/// Runs the message loop.
///
/// Executes previously [`spawn`]ed tasks.
///
/// # Panics
///
/// Panics when the message loop is running already. This happens when
/// `block_on` or `run` is called from async tasks running on this executor.
pub fn run_message_loop() {
    run_message_loop_with_dispatcher(|_| false);
}

/// Runs the message loop, calling `dispatcher` for each received message.
///
/// If `dispatcher` has handled the message it shall return true. When returning
/// `false` the message is forwarded to the default dispatcher.
///
/// When using `backend-async-task` the message 0xB43A (WM_APP + 13370) is
/// reserved. Messages with that number will be handled and filtered by the
/// executor backend.
///
/// Executes previously [`spawn`]ed tasks.
///
/// # Panics
///
/// Panics when the message loops is running already. This happens when
/// `block_on` or `run` is called from async tasks running on this executor.
pub fn run_message_loop_with_dispatcher(dispatcher: impl Fn(&MSG) -> bool) {
    thread_local!(static MESSAGE_LOOP_RUNNING: Cell<bool> = const { Cell::new(false) });
    assert!(
        !MESSAGE_LOOP_RUNNING.replace(true),
        "a message loop is running already"
    );

    // Any modal window (i.e. a right-click menu) blocks the main message loop
    // and dispatches messages internally. To keep the executor running use a
    // hook to get access to modal windows internal message loop.
    // SAFETY: The Drop implementation of MsgFilterHook unregisters the hook,
    // ensuring that dispatchers will not be called after the end of the scope.
    let _hook =
        unsafe { MsgFilterHook::register(move |msg| backend::dispatch(msg) || dispatcher(msg)) };

    loop {
        let mut msg = MaybeUninit::uninit();
        unsafe {
            let ret = GetMessageA(msg.as_mut_ptr(), ptr::null_mut(), 0, 0);
            let msg = msg.assume_init();
            match ret {
                1 => {
                    // Handle the message in the msg filter hook.
                    if CallMsgFilterA(&msg, 0) == 0 {
                        TranslateMessage(&msg);
                        DispatchMessageA(&msg);
                    }
                }
                0 => break,
                _ => unreachable!(),
            }
        }
    }

    MESSAGE_LOOP_RUNNING.set(false);
}

/// Quits the current threads message loop.
pub fn quit_message_loop() {
    unsafe { PostQuitMessage(0) };
}

/// Returned by [`block_on()`] when [`quit_message_loop()`] was called.
#[derive(Debug, Clone, Copy)]
pub struct QuitMessageLoop;

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
pub fn block_on<F>(future: F) -> Result<F::Output, QuitMessageLoop>
where
    F: Future + 'static,
    F::Output: 'static,
{
    // Wrap the future so it quits the message loop when finished.
    let task = backend::spawn(async move {
        let result = future.await;
        quit_message_loop();
        result
    });
    run_message_loop();
    poll_ready(task).map_err(|_| QuitMessageLoop)
}

fn poll_ready<T>(future: impl Future<Output = T>) -> Result<T, ()> {
    // TODO: wait for https://github.com/rust-lang/rust/issues/98286 to land.
    const NOOP_WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
        |_| RawWaker::new(ptr::null(), &NOOP_WAKER_VTABLE),
        |_| (),
        |_| (),
        |_| (),
    );
    let noop_waker = unsafe { Waker::from_raw(RawWaker::new(ptr::null(), &NOOP_WAKER_VTABLE)) };
    let future = pin!(future);
    if let Poll::Ready(result) = future.poll(&mut Context::from_waker(&noop_waker)) {
        Ok(result)
    } else {
        Err(())
    }
}

/// An owned permission to join on a task (await its termination).
///
/// If a `JoinHandle` is dropped, then its task continues running in the
/// background and its return value is lost.
pub type JoinHandle<F> = backend::JoinHandle<F>;

/// Spawns a new future on the current thread.
///
/// This function may be used to spawn tasks when the message loop is not
/// running. The provided future will start running once the message loop
/// is entered with [`run_message_loop`], [`run_message_loop_with_dispatcher`]
/// or [`block_on`].
pub fn spawn<F>(future: F) -> JoinHandle<F>
where
    F: Future + 'static,
    F::Output: 'static,
{
    backend::spawn(future)
}
