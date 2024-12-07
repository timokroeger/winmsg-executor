#![doc = include_str!("../README.md")]

pub mod util;

use std::{
    any::Any,
    cell::Cell,
    future::Future,
    mem::{ManuallyDrop, MaybeUninit},
    panic,
    pin::{pin, Pin},
    ptr::{self, NonNull},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

use async_task::Runnable;
use util::{MsgFilterHook, Window, WindowType};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

thread_local! {
    pub(crate) static PANIC_PAYLOAD: Cell<Option<Box<dyn Any + Send + 'static>>>
        = const { Cell::new(None) };
}

const MSG_ID_WAKE: u32 = WM_USER;

/// An owned permission to join on a task (await its termination).
///
/// If a `JoinHandle` is dropped, then its task continues running in the
/// background and its return value is lost.
pub struct JoinHandle<T> {
    task: ManuallyDrop<async_task::Task<T>>,
}

// Keep the task running when dropped.
impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        let task = unsafe { ManuallyDrop::take(&mut self.task) };
        task.detach();
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        pin!(&mut *self.task).poll(cx)
    }
}

fn spawn_unchecked<'a, T: 'a>(future: impl Future<Output = T> + 'a) -> JoinHandle<T> {
    // Create a message only window to run the tasks.
    let window = Window::new_reentrant(WindowType::MessageOnly, (), |_, msg| {
        if msg.msg == MSG_ID_WAKE {
            let runnable =
                unsafe { Runnable::<()>::from_raw(NonNull::new_unchecked(msg.lparam as *mut _)) };
            runnable.run();
            Some(0)
        } else {
            None
        }
    })
    .unwrap();

    // SAFETY:
    // * The `future` does not need to be `Send` because the thread that receives the runnable is
    //   our own, meaning the runniable is also dropped on original thread.
    let (runnable, task) = unsafe {
        async_task::spawn_unchecked(future, move |runnable: Runnable| {
            PostMessageA(
                window.hwnd(),
                MSG_ID_WAKE,
                0,
                runnable.into_raw().as_ptr() as _,
            );
        })
    };

    // Trigger initial poll.
    runnable.schedule();

    JoinHandle {
        task: ManuallyDrop::new(task),
    }
}

/// Spawns a new future on the current thread.
///
/// This function may be used to spawn tasks when the message loop is not
/// running. The provided future will start running once the message loop
/// is entered with [`run_message_loop`], [`run_message_loop_with_dispatcher`]
/// or [`block_on`].
pub fn spawn<T>(future: impl Future<Output = T> + 'static) -> JoinHandle<T> {
    spawn_unchecked(future)
}

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
    let _hook = unsafe { MsgFilterHook::register(dispatcher) };

    loop {
        let mut msg = MaybeUninit::uninit();
        unsafe {
            let ret = GetMessageA(msg.as_mut_ptr(), ptr::null_mut(), 0, 0);
            let msg = msg.assume_init();
            match ret {
                1 => {
                    // Handle the message in the msg filter hook.
                    if CallMsgFilterA(&msg, MSGF_USER as _) == 0 {
                        if let Some(panic_payload) = PANIC_PAYLOAD.take() {
                            panic::resume_unwind(panic_payload)
                        }
                        TranslateMessage(&msg);
                        DispatchMessageA(&msg);
                    }
                    if let Some(panic_payload) = PANIC_PAYLOAD.take() {
                        panic::resume_unwind(panic_payload)
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
pub fn block_on<'a, T: 'a>(future: impl Future<Output = T> + 'a) -> Result<T, QuitMessageLoop> {
    // Wrap the future so it quits the message loop when finished.
    let task = spawn_unchecked(async move {
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

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    #[should_panic]
    fn panic_in_dispatcher() {
        unsafe { PostMessageA(ptr::null_mut(), WM_USER, 0, 0) };
        run_message_loop_with_dispatcher(|_| panic!());
    }
}
