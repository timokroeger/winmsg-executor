#![doc = include_str!("../README.md")]

pub mod util;

use std::{
    cell::{Cell, UnsafeCell},
    future::Future,
    marker::PhantomData,
    mem::{self, MaybeUninit},
    pin::{pin, Pin},
    ptr,
    sync::Arc,
    task::{Context, Poll, RawWaker, RawWakerVTable, Wake, Waker},
};

use util::Window;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::util::MsgFilterHook;

const MSG_ID_WAKE: u32 = WM_USER;

// Same terminology as the `async-task` crate.
enum TaskState<F: Future> {
    Running(F, Option<Waker>),
    Completed(F::Output),
    Closed,
}

struct Task<F: Future> {
    window: Window<()>,
    state: UnsafeCell<TaskState<F>>,
}

// SAFETY: The wake implementation (which requires `Send` and `Sync`) only uses
// the window handle and passes it to a safe function call. All other state is
// only accessed from one thread.
unsafe impl<F: Future> Send for Task<F> {}
unsafe impl<F: Future> Sync for Task<F> {}

impl<F: Future> Wake for Task<F> {
    fn wake(self: Arc<Self>) {
        // Ideally the waker would know if the task has completed to decide if
        // its necessary to send a wake message. But that also means access that
        // task state must be made thread safe. Instead, always post the wake
        // message and let the receiver side (which runs on the same thread the
        // task was created on) decide if a task needs to be polled.
        // `Arc<Self>` keeps the target window alive for as long as wakers for
        // the task exist.
        unsafe {
            PostMessageA(
                self.window.hwnd(),
                MSG_ID_WAKE,
                0,
                Arc::into_raw(self) as isize,
            )
        };
    }
}

/// An owned permission to join on a task (await its termination).
///
/// If a `JoinHandle` is dropped, then its task continues running in the
/// background and its return value is lost.
pub struct JoinHandle<F: Future> {
    task: Arc<Task<F>>,
    _not_send: PhantomData<*const ()>,
}

impl<F: Future> Future for JoinHandle<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let task_state = unsafe { &mut *self.task.state.get() };

        if let TaskState::Running(_, waker) = task_state {
            match waker {
                Some(waker) if waker.will_wake(cx.waker()) => {}
                waker => *waker = Some(cx.waker().clone()),
            }
            return Poll::Pending;
        }

        if let TaskState::Completed(result) = mem::replace(task_state, TaskState::Closed) {
            Poll::Ready(result)
        } else {
            panic!("future polled after ready");
        }
    }
}

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
    // Create a message only window to run the tasks.
    let window = Window::new_reentrant(true, (), |_, msg| {
        if msg.msg == MSG_ID_WAKE {
            // Poll the tasks future
            let task = unsafe { Arc::from_raw(msg.lparam as *const Task<F>) };
            let task_state = unsafe { &mut *task.state.get() };

            if let TaskState::Running(ref mut future, ref mut waker) = task_state {
                let future_pinned = unsafe { Pin::new_unchecked(future) };
                if let Poll::Ready(result) =
                    future_pinned.poll(&mut Context::from_waker(&Waker::from(task.clone())))
                {
                    if let Some(w) = waker.take() {
                        w.wake();
                    }
                    *task_state = TaskState::Completed(result);
                }
            }

            Some(0)
        } else {
            None
        }
    })
    .unwrap();

    let task = Arc::new(Task {
        window,
        state: UnsafeCell::new(TaskState::Running(future, None)),
    });

    // Trigger initial poll.
    Waker::from(task.clone()).wake();

    JoinHandle {
        task,
        _not_send: PhantomData,
    }
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
    let task = spawn(async move {
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
