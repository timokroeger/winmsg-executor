use std::{
    future::Future,
    mem::ManuallyDrop,
    pin::{pin, Pin},
    ptr::NonNull,
    task::{Context, Poll},
};

use async_task::{Runnable, Schedule};
use windows_sys::Win32::{System::Threading::GetCurrentThreadId, UI::WindowsAndMessaging::*};

const MSG_ID_WAKE: u32 = WM_APP + 13370;

pub fn dispatch(msg: &MSG) -> bool {
    // Only accept the wake message if it was posted to the message loop directly (hwnd == 0).
    if msg.hwnd.is_null() && msg.message == MSG_ID_WAKE {
        let runnable =
            unsafe { Runnable::<()>::from_raw(NonNull::new_unchecked(msg.lParam as *mut _)) };
        runnable.run();
        true
    } else {
        false
    }
}

pub fn spawn<F>(future: F) -> JoinHandle<F>
where
    F: Future + 'static,
    F::Output: 'static,
{
    // It's important to get the current thread id *outside* of the `schedule`
    // closure which may run from different thread.
    let thread_id = unsafe { GetCurrentThreadId() };

    // To schedule the task, we post the runnable to our own thread's message
    // queue. It is safe safe to keep waker references in different threads even
    // after the message loop thread has terminated, because `async-task` does
    // not call the schedule closure for completed/canceled tasks.
    let schedule = move |runnable: Runnable| unsafe {
        PostThreadMessageA(thread_id, MSG_ID_WAKE, 0, runnable.into_raw().as_ptr() as _);
    };

    let (runnable, task) = spawn_local(future, schedule);

    // Trigger a first poll.
    runnable.schedule();

    JoinHandle {
        task: ManuallyDrop::new(task),
    }
}

fn spawn_local<F, S>(future: F, schedule: S) -> (Runnable, async_task::Task<F::Output>)
where
    F: Future + 'static,
    F::Output: 'static,
    S: Schedule + Send + Sync + 'static,
{
    // SAFETY: The `future` does not need to be `Send` because the thread that
    // receives the runnable is our own. All other safety properties are ensured
    // by the function signature.
    unsafe { async_task::spawn_unchecked(future, schedule) }
}

// Use a newtype around `async-task` task type to adjust its drop behavior.
pub struct JoinHandle<F: Future> {
    task: ManuallyDrop<async_task::Task<F::Output>>,
}

// Keep the task running when dropped.
impl<F: Future> Drop for JoinHandle<F> {
    fn drop(&mut self) {
        let task = unsafe { ManuallyDrop::take(&mut self.task) };
        task.detach();
    }
}

impl<F: Future> Future for JoinHandle<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        pin!(&mut *self.task).poll(cx)
    }
}
