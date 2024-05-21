use std::{future::Future, ptr::NonNull};

use async_task::Runnable;
use windows_sys::Win32::{Foundation::*, System::Threading::*, UI::WindowsAndMessaging::*};

const MSG_ID_WAKE: u32 = WM_NULL;

pub fn run() {
    // Any modal window (i.e. a right-click menu) blocks the main message loops
    // and dispatches messages internally. To keep the executor running use a
    // hook to get access to modal windows internal message loop.
    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        println!("in hook code={}", code);
        if code >= 0 && dispatch(&*(lparam as *const MSG)) {
            -1
        } else {
            CallNextHookEx(0, code, wparam, lparam)
        }
    }
    let hook = unsafe { SetWindowsHookExA(WH_MSGFILTER, Some(hook_proc), 0, GetCurrentThreadId()) };
    crate::run_message_loop();
    unsafe { UnhookWindowsHookEx(hook) };
}

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

pub fn spawn(future: impl Future<Output = ()> + 'static) {
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
