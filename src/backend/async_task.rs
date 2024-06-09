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
    // Ignore any window messages (hwnd != 0) and look at wake messages messages
    // sent to the message loop directly (hwnd == 0).
    if msg.hwnd == 0 && msg.message == MSG_ID_WAKE {
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
    // Its important to get the current thread id *outside* of the `schedule`
    // closure which can run from a different.
    let thread_id = unsafe { GetCurrentThreadId() };

    // To schedule the task we post the runnable to our own threads message
    // queue. `async-task` tracks if the task has completed and prevents the
    // schedule closure from being called. That means its safe to keep waker
    // references in different threads even after a task has completed and its
    // executing thread was terminated.
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
