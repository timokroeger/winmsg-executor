use std::{
    cell::UnsafeCell,
    future::Future,
    mem,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::util::Window;

pub const fn dispatch(_msg: &MSG) -> bool {
    // Forward all message and let the operating system handle dispatching of
    // messages to the matching wndproc.
    false
}

const MSG_ID_WAKE: u32 = WM_USER;

// Same terminology as `async-task` crate.
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

pub struct JoinHandle<F: Future> {
    task: Arc<Task<F>>,
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

    JoinHandle { task }
}
