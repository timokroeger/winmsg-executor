use std::{
    cell::Cell,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

use windows_sys::Win32::{Foundation::*, UI::WindowsAndMessaging::*};

use crate::util::create_window;

pub fn dispatch(_msg: &MSG) -> bool {
    // Forward all message and let windows handle the dispatching of messages
    // to each tasks wndproc.
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

// SAFETY: State is only accessed from one thread.
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
                let waker = waker
                    .map(|mut w| {
                        w.clone_from(cx.waker());
                        w
                    })
                    .unwrap_or_else(|| cx.waker().clone());
                task_state.set(TaskState::Running(future, Some(waker)));
                Poll::Pending
            }
            TaskState::Finished(result) => Poll::Ready(result),
        }
    }
}

pub fn spawn<T: 'static>(future: impl Future<Output = T> + 'static) -> Task<T> {
    // Create a message only window to run the taks.
    let hwnd = create_window(Box::new(
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
                        result_waker.map(Waker::wake);
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
    ));

    let task = Arc::new(TaskInner::new(hwnd, future));

    // Trigger initial poll.
    Waker::from(task.clone()).wake();

    Task(task)
}
