#![doc = include_str!("../README.md")]

pub mod util;

use std::{
    cell::Cell,
    future::Future,
    mem::{ManuallyDrop, MaybeUninit},
    pin::{pin, Pin},
    ptr::{self, NonNull},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

use async_task::Runnable;
use util::{Window, WindowType};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::util::MsgFilterHook;

const MSG_ID_WAKE: u32 = WM_USER;

thread_local! {
    static EXECUTOR_WINDOW: Window<()> = Window::new(WindowType::MessageOnly, (), |_, msg| {
        if msg.msg == MSG_ID_WAKE {
            let runnable = unsafe {
                let runnable_ptr = NonNull::new_unchecked(msg.lparam as *mut _);
                Runnable::<()>::from_raw(runnable_ptr)
            };
            runnable.run();
            Some(0)
        } else {
            None
        }
    })
    .unwrap();
}

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

unsafe fn spawn_unchecked_lifetime<T>(future: impl Future<Output = T>) -> JoinHandle<T> {
    let hwnd = EXECUTOR_WINDOW.with(|w| w.hwnd());

    // SAFETY: The `future` does not need to be `Send` because the thread that
    // receives the runnable is our own, meaning the runniable is also dropped
    // on original thread.
    let (runnable, task) = unsafe {
        async_task::spawn_unchecked(future, move |runnable: Runnable| {
            PostMessageA(hwnd, MSG_ID_WAKE, 0, runnable.into_raw().as_ptr() as _);
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
/// is entered with [`block_on`] or [`MessageLoop::run`].
pub fn spawn_local<T>(future: impl Future<Output = T> + 'static) -> JoinHandle<T> {
    // SAFETY: future is `'static`
    unsafe { spawn_unchecked_lifetime(future) }
}

/// Runs a future to completion on the calling threads message loop.
///
/// This runs the provided future on the current thread, blocking until it is
/// complete. Also runs any tasks [`spawn`]ed from the same thread. Note that
/// any spawned tasks will be suspended after `block_on` returns. Calling
/// `block_on` again will resume previously spawned tasks.
///
/// # Panics
///
/// Panics when quitting out of the message loop without the future being
/// ready. This can happen when calling when the future or any spawned task
/// calls the `PostQuitMessage()` winapi function.
pub fn block_on<'a, T: 'a>(future: impl Future<Output = T> + 'a) -> T {
    let msg_loop = &MessageLoop::new();

    // Wrap the future so it quits the message loop when finished.
    // SAFETY: All borrowed variables outlive the task itself because we only
    // return from this function after the task has finished.
    let task = unsafe {
        spawn_unchecked_lifetime(async move {
            let result = future.await;
            msg_loop.quit();
            result
        })
    };

    msg_loop.run_loop(|_| FilterResult::Forward);

    poll_ready(task).expect("received unexpected quit message")
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

/// Return value of the filter closure passed to [`MessageLoop::run`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterResult {
    /// The message is forwarded to the window procedure.
    Forward,

    /// The message is dropped and not forwarded to the window procedure.
    Drop,
}

/// Abstract representation of a message loop.
///
/// Not directly constructible, use [`MessageLoop::run`] to create a message
/// loop. The message loop struct is used to control the message loop behavior
/// by passing it as an argument to the filter closure of [`MessageLoop::run`].
pub struct MessageLoop {
    quit: Cell<bool>,
}

impl MessageLoop {
    fn new() -> Self {
        Self {
            quit: Cell::new(false),
        }
    }

    fn run_loop(&self, filter: impl Fn(&MSG) -> FilterResult) {
        while !self.quit.get() {
            unsafe {
                let mut msg = MaybeUninit::uninit();
                if GetMessageA(msg.as_mut_ptr(), ptr::null_mut(), 0, 0) == 0 {
                    return;
                }
                let msg = msg.assume_init();

                if filter(&msg) == FilterResult::Forward {
                    TranslateMessage(&msg);
                    DispatchMessageA(&msg);
                }
            }
        }
    }

    /// Runs the message loop with a filter closure to inspect and drop messages
    /// before they are dispatched to their respective window procedure.
    ///
    /// Use the [`FilterResult`] return value to control how the message is
    /// handled. The first argument to the filter closure is the [`MessageLoop`]
    /// struct itself, which can be used to quit out of the message loop.
    ///
    /// Like [`block_on`] this function runs any tasks [`spawn`]ed from the same
    /// thread. Any spawned tasks will be suspended when the `run_message_loop`
    /// returns. Be careful not to drop messages not belonging to a window you
    /// control or you might risk suspending a task indefinitely when dropping
    /// its wake message.
    ///
    /// `run_message_loop` installs a [`WH_MSGFILTER`] hook to allow inspections
    /// of messages while modal windows are open.
    ///
    /// # Panics and Reentrancy
    ///
    /// Panics when called from within another `run_message_loop` filter closure.
    /// A call to [`block_on()`] from within the filter closure creates a nested
    /// message loop which causes the filter closure to be reentered when a modal
    /// window is open.
    ///
    /// [`WH_MSGFILTER`]: (https://learn.microsoft.com/en-us/windows/win32/winmsg/about-hooks#wh_msgfilter-and-wh_sysmsgfilter)
    pub fn run(filter: impl Fn(&MessageLoop, &MSG) -> FilterResult) {
        let msg_loop = MessageLoop::new();

        // Any modal window (i.e. a right-click menu) blocks the main message loop
        // and dispatches messages internally. To keep the executor running use a
        // hook to get access to modal windows' internal message loop.
        // SAFETY: The Drop implementation of MsgFilterHook unregisters the hook,
        // ensuring that dispatchers will not be called after the end of the scope.
        let _hook =
            unsafe { MsgFilterHook::register(|msg| filter(&msg_loop, msg) == FilterResult::Drop) };

        msg_loop.run_loop(|msg| filter(&msg_loop, msg));
    }

    /// Quits the message loop as soon as possible.
    pub fn quit(&self) {
        self.quit.set(true);
    }

    /// Quits the message loop when there are no more messages to process.
    pub fn quit_when_idle(&self) {
        unsafe { PostQuitMessage(0) };
    }
}

#[cfg(test)]
mod test {
    use std::{ffi::CStr, future::poll_fn};

    use windows_sys::Win32::Foundation::HWND;

    use super::*;

    fn post_thread_message(msg: u32) {
        unsafe { PostMessageA(ptr::null_mut(), msg, 0, 0) };
    }

    #[test]
    #[should_panic]
    fn panic_in_dispatcher() {
        post_thread_message(WM_USER);
        MessageLoop::run(|_, _| panic!());
    }

    #[test]
    fn message_loop_quit() {
        for i in 0..10 {
            post_thread_message(WM_USER + i);
        }
        MessageLoop::run(|msg_loop, msg| {
            // This is the only ever message we observe becasue we quit the
            // loop right after it is received.
            assert_eq!(msg.message, WM_USER);
            msg_loop.quit();
            FilterResult::Drop
        });
    }

    #[test]
    fn message_loop_quit_when_idle() {
        for i in 0..10 {
            post_thread_message(WM_USER + i);
        }
        let expected_msg = Cell::new(0);
        MessageLoop::run(|msg_loop, msg| {
            assert_eq!(msg.message, WM_USER + expected_msg.get());
            expected_msg.set(expected_msg.get() + 1);
            msg_loop.quit_when_idle();
            FilterResult::Drop
        });
        assert_eq!(expected_msg.get(), 10);
    }

    #[test]
    fn nested_block_on() {
        let count: Cell<usize> = Cell::new(0);

        block_on(async {
            assert_eq!(count.get(), 0);
            count.set(count.get() + 1);

            block_on(async {
                assert_eq!(count.get(), 1);
                count.set(count.get() + 1);
            });

            assert_eq!(count.get(), 2);
            count.set(count.get() + 1);
        });

        assert_eq!(count.get(), 3);
    }

    #[test]
    #[should_panic]
    fn nested_message_loop() {
        post_thread_message(WM_USER);
        MessageLoop::run(|_, _| {
            MessageLoop::run(|_, _| FilterResult::Drop);
            FilterResult::Drop
        });
    }

    async fn yield_now() {
        let mut yielded = false;
        poll_fn(|cx| {
            if yielded {
                Poll::Ready(())
            } else {
                yielded = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
    }

    #[test]
    fn nested_message_loop_block_on() {
        let inner_executed = Cell::new(false);

        post_thread_message(WM_USER);
        MessageLoop::run(|msg_loop, _| {
            block_on(async {
                inner_executed.set(true);
            });
            msg_loop.quit();
            FilterResult::Forward
        });

        assert!(inner_executed.get());
    }

    #[test]
    fn nested_message_loop_block_on_quit() {
        post_thread_message(WM_USER);
        MessageLoop::run(|msg_loop, _| {
            block_on(async {
                msg_loop.quit();
            });
            FilterResult::Forward
        });
    }

    fn window_by_name(name: &CStr) -> HWND {
        unsafe { FindWindowA(ptr::null_mut(), name.as_ptr() as _) }
    }

    #[test]
    fn running_spawned_with_modal_dialog() {
        // The window name must be unique for each test because cargo runs tests
        // in parallel and we do not want to close the window of another test.
        let window_name = c"running_spawned_with_modal_dialog";

        let task = spawn_local(async {
            // Wait for modal window to be open.
            while window_by_name(window_name).is_null() {
                yield_now().await;
            }

            // Do some async work with modal dialog open.
            for _ in 0..10 {
                yield_now().await;
            }

            // Close the modal window.
            unsafe {
                SendMessageA(window_by_name(window_name), WM_CLOSE, 0, 0);
            }
        });

        block_on(async {
            unsafe {
                MessageBoxA(
                    ptr::null_mut(),
                    ptr::null_mut(),
                    window_name.as_ptr() as _,
                    0,
                );
            }
            task.await;
        });
    }

    #[test]
    fn message_loop_with_modal_dialog() {
        // The window name must be unique for each test because cargo runs tests
        // in parallel and we do not want to close the window of another test.
        let window_name = c"message_loop_with_modal_dialog";

        spawn_local(async {
            unsafe {
                MessageBoxA(
                    ptr::null_mut(),
                    ptr::null_mut(),
                    window_name.as_ptr() as _,
                    0,
                );
            }
        });

        spawn_local(async {
            // Check if modal window is actually open.
            assert!(!window_by_name(window_name).is_null());

            for i in 0..10 {
                post_thread_message(WM_USER + i);
                yield_now().await;
            }

            // Close modal window again.
            unsafe { SendMessageA(window_by_name(window_name), WM_CLOSE, 0, 0) };
        });

        let expected_msg = Cell::new(0);
        MessageLoop::run(|msg_loop, msg| {
            if msg.hwnd.is_null() && msg.message >= WM_USER {
                assert_eq!(msg.message, WM_USER + expected_msg.get());
                expected_msg.set(expected_msg.get() + 1);
                msg_loop.quit_when_idle();
                FilterResult::Drop
            } else {
                FilterResult::Forward
            }
        });
        assert_eq!(expected_msg.get(), 10);
    }
}
