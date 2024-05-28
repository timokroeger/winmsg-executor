use std::{
    cell::Cell,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

use windows_sys::Win32::{Foundation::*, UI::WindowsAndMessaging::*};

use crate::util::create_window;

pub const fn dispatch(_msg: &MSG) -> bool {
    // Forward all message and let the operating system handle dispatching of
    // messages to the matching wndproc.
    false
}

const MSG_ID_WAKE: u32 = WM_USER;

enum TaskState<T> {
    Invalid,
    Running(Pin<Box<dyn Future<Output = T>>>, Option<Waker>),
    Finished(T),
}

struct TaskInner<T> {
    hwnd: HWND,
    state: Cell<TaskState<T>>,
}

// SAFETY: The wake implementation (which requires `Send` and `Sync`) only uses
// the window handle and passes it to a safe function call. All other state is
// only accessed from one thread.
unsafe impl<T> Send for TaskInner<T> {}
unsafe impl<T> Sync for TaskInner<T> {}

impl<T> TaskInner<T> {
    fn new(hwnd: HWND, future: impl Future<Output = T> + 'static) -> Self {
        Self {
            hwnd,
            state: Cell::new(TaskState::Running(Box::pin(future), None)),
        }
    }
}

impl<T> Wake for TaskInner<T> {
    fn wake(self: Arc<Self>) {
        // Ideally the waker would know if the task has completed to decide if
        // its necessary to send a wake message. But that also means access to
        // task state must be made thread safe. Instead, always post the wake
        // message and let the receiver side (which runs on the same thread the
        // task was created on) decide if a task needs to be polled.
        // `Arc<Self>` keeps the target window alive for as long as wakers for
        // the task exist.
        unsafe { PostMessageA(self.hwnd, MSG_ID_WAKE, 0, Arc::into_raw(self) as isize) };
    }
}

impl<T> Drop for TaskInner<T> {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.hwnd) };
    }
}

pub struct Task<T>(Arc<TaskInner<T>>);

impl<T> Future for Task<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let task_state = &self.0.state;
        match task_state.replace(TaskState::Invalid) {
            TaskState::Invalid => panic!(),
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
            TaskState::Finished(result) => Poll::Ready(result),
        }
    }
}

pub fn spawn<T: 'static>(future: impl Future<Output = T> + 'static) -> Task<T> {
    // Create a message only window to run the tasks.
    let hwnd = create_window(
        true,
        |_hwnd: HWND, msg: u32, _wparam: WPARAM, lparam: LPARAM| {
            if msg == MSG_ID_WAKE {
                // Poll the tasks future
                let task = unsafe { Arc::from_raw(lparam as *const TaskInner<T>) };
                if let TaskState::Running(mut future, result_waker) =
                    task.state.replace(TaskState::Invalid)
                {
                    let new_state = if let Poll::Ready(result) = future
                        .as_mut()
                        .poll(&mut Context::from_waker(&Waker::from(task.clone())))
                    {
                        if let Some(w) = result_waker {
                            w.wake();
                        }
                        TaskState::Finished(result)
                    } else {
                        TaskState::Running(future, result_waker)
                    };
                    task.state.set(new_state);
                }
                Some(0)
            } else {
                None
            }
        },
    );

    let task = Arc::new(TaskInner::new(hwnd, future));

    // Trigger initial poll.
    Waker::from(task.clone()).wake();

    Task(task)
}
