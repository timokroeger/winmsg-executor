use std::{future::Future, ptr::NonNull, rc::Rc};

use async_task::Runnable;
use windows_sys::Win32::{Foundation::*, System::Threading::*, UI::WindowsAndMessaging::*};

use crate::QuitMessageLoopOnDrop;

const MSG_ID_WAKE: u32 = WM_NULL;

pub fn dispatch(msg: &MSG) -> bool {
    if msg.hwnd == 0 && msg.message == MSG_ID_WAKE {
        let runnable =
            unsafe { Runnable::<()>::from_raw(NonNull::new(msg.lParam as *mut _).unwrap()) };
        runnable.run();
        true
    } else {
        false
    }
}

pub fn spawn(msg_loop: Rc<QuitMessageLoopOnDrop>, future: impl Future<Output = ()> + 'static) {
    let mut thread_handle = 0;
    let thread_id = unsafe {
        let process_handle = GetCurrentProcess();
        DuplicateHandle(
            process_handle,
            GetCurrentThread(),
            process_handle,
            &mut thread_handle,
            0,
            FALSE,
            DUPLICATE_SAME_ACCESS,
        );
        GetThreadId(thread_handle)
    };

    let future = async move {
        // Keep the message loop alive as long as the future runs.
        let _msg_loop = msg_loop;
        future.await;
        unsafe { CloseHandle(thread_handle) };
    };

    let schedule = move |runnable: Runnable| unsafe {
        PostThreadMessageW(thread_id, MSG_ID_WAKE, 0, runnable.into_raw().as_ptr() as _);
    };

    let (runnable, task) = async_task::spawn_local(future, schedule);
    task.detach();
    runnable.schedule();
}
