use std::{
    cell::Cell,
    future::Future,
    pin::Pin,
    rc::Rc,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

use windows_sys::Win32::{Foundation::*, UI::WindowsAndMessaging::*};

use crate::window::create_window;

pub fn dispatch(_msg: &MSG) -> bool {
    // Forward all message and let windows handle the dispatching of messages
    // to each tasks wndproc.
    false
}

const MSG_ID_WAKE: u32 = WM_USER;

struct WindowWaker(HWND);

impl Wake for WindowWaker {
    fn wake(self: std::sync::Arc<Self>) {
        unsafe { PostMessageA(self.0, MSG_ID_WAKE, 0, 0) };
    }
}

impl Drop for WindowWaker {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.0) };
    }
}

pub fn spawn<T: 'static>(future: impl Future<Output = T> + 'static) -> Task<T> {
    let task = Task(Rc::new(Cell::new(TaskState::Running)));
    let task_state = task.0.clone();

    // Create a message only window to run the taks.
    create_window(Box::new({
        let mut future = Box::pin(future);
        let mut waker = None;
        move |hwnd: HWND, msg: u32, _wparam: WPARAM, _lparam: LPARAM| {
            if msg == WM_CREATE {
                waker = Some(Waker::from(Arc::new(WindowWaker(hwnd))));
            }

            if msg == WM_CREATE || msg == MSG_ID_WAKE {
                // Poll the tasks future
                if let Poll::Ready(result) = future
                    .as_mut()
                    .poll(&mut Context::from_waker(waker.as_ref().unwrap()))
                {
                    // Remove this tasks waker reference.
                    if let TaskState::RunningWaiting(result_waker) =
                        task_state.replace(TaskState::Finished(result))
                    {
                        result_waker.wake();
                    }
                    waker = None;
                }
                Some(0)
            } else {
                None
            }
        }
    }));

    task
}

enum TaskState<T> {
    Invalid,
    Running,
    RunningWaiting(Waker),
    Finished(T),
}

pub struct Task<T>(Rc<Cell<TaskState<T>>>);

impl<T> Future for Task<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let task_state = &self.0;
        match task_state.replace(TaskState::Invalid) {
            TaskState::Invalid => panic!(),
            TaskState::Running => {
                task_state.set(TaskState::RunningWaiting(cx.waker().clone()));
                Poll::Pending
            }
            TaskState::RunningWaiting(mut waker) => {
                waker.clone_from(cx.waker());
                task_state.set(TaskState::RunningWaiting(waker));
                Poll::Pending
            }
            TaskState::Finished(result) => Poll::Ready(result),
        }
    }
}
