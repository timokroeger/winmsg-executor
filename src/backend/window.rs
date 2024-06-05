use std::{
    cell::Cell,
    future::Future,
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

// Use same terminology as `async-task` crate.
enum TaskState<F: Future> {
    Running(F, Option<Waker>),
    Completed(F::Output),
    Closed,
}

struct Task<F: Future> {
    window: Window<()>,
    state: Cell<TaskState<F>>,
}

// SAFETY: The wake implementation (which requires `Send` and `Sync`) only uses
// the window handle and passes it to a safe function call. All other state is
// only accessed from one thread.
unsafe impl<F: Future> Send for Task<F> {}
unsafe impl<F: Future> Sync for Task<F> {}

impl<F: Future> Wake for Task<F> {
    fn wake(self: Arc<Self>) {
        // Ideally the waker would know if the task has completed to decide if
        // its necessary to send a wake message. But that also means access to
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
        let task_state = &self.task.state;
        match task_state.replace(TaskState::Closed) {
            TaskState::Closed => panic!(),
            TaskState::Running(future, waker) => {
                let waker = waker.map_or_else(
                    || cx.waker().clone(),
                    |mut w| {
                        w.clone_from(cx.waker());
                        w
                    },
                );
                task_state.set(TaskState::Running(future, Some(waker)));
                Poll::Pending
            }
            TaskState::Completed(result) => Poll::Ready(result),
        }
    }
}

pub fn spawn<F: Future + 'static>(future: F) -> JoinHandle<F> {
    // Create a message only window to run the tasks.
    let window = Window::new_reentrant(true, (), |_, msg| {
        if msg.msg == MSG_ID_WAKE {
            // Poll the tasks future
            let task = unsafe { Arc::from_raw(msg.lparam as *const Task<F>) };
            if let TaskState::Running(mut future, result_waker) =
                task.state.replace(TaskState::Closed)
            {
                let future_pinned = unsafe { Pin::new_unchecked(&mut future) };
                let new_state = if let Poll::Ready(result) =
                    future_pinned.poll(&mut Context::from_waker(&Waker::from(task.clone())))
                {
                    if let Some(w) = result_waker {
                        w.wake();
                    }
                    TaskState::Completed(result)
                } else {
                    TaskState::Running(future, result_waker)
                };
                task.state.set(new_state);
            }
            Some(0)
        } else {
            None
        }
    })
    .unwrap();

    let task = Arc::new(Task {
        window,
        state: Cell::new(TaskState::Running(future, None)),
    });

    // Trigger initial poll.
    Waker::from(task.clone()).wake();

    JoinHandle { task }
}
