use std::{future::Future, ptr::NonNull};

use async_task::Runnable;
use windows_sys::Win32::{System::Threading::GetCurrentThreadId, UI::WindowsAndMessaging::*};

const MSG_ID_WAKE: u32 = WM_APP;

pub fn dispatch(msg: &MSG) -> bool {
    // Only look at messages sent by `PostThreadMessageA()`
    if msg.hwnd == 0 && msg.message == MSG_ID_WAKE {
        let runnable =
            unsafe { Runnable::<()>::from_raw(NonNull::new_unchecked(msg.lParam as *mut _)) };
        runnable.run();
        true
    } else {
        false
    }
}

pub fn spawn(future: impl Future<Output = ()> + 'static) {
    // Its important to get the current thread id *outside* of the `schedule`
    // closure which can run from a different.
    let thread_id = unsafe { GetCurrentThreadId() };

    // To schedule the task we post the runnable to our own threads message queue.
    let schedule = move |runnable: Runnable| unsafe {
        PostThreadMessageA(thread_id, MSG_ID_WAKE, 0, runnable.into_raw().as_ptr() as _);
    };

    // SAFETY: The `future` does not need to be `Send` because the thread that receives
    // the runnable is our own. All other safety properties are ensured at compiletime.
    fn is_send_sync<T: Send + Sync + 'static>(_: &T) {}
    is_send_sync(&schedule);
    let (runnable, task) = unsafe { async_task::spawn_unchecked(future, schedule) };

    // We are not interested in the futures return value.
    task.detach();

    // Trigger a first poll.
    runnable.schedule();
}
