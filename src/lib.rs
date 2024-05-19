pub mod window;

use std::{
    cell::Cell,
    future::Future,
    marker::PhantomData,
    mem::MaybeUninit,
    pin::Pin,
    rc::Rc,
    task::{Context, RawWaker, RawWakerVTable, Waker},
};

use window::{create_window, WindowContext};
use windows_sys::Win32::{Foundation::*, UI::WindowsAndMessaging::*};

const MSG_ID_WAKE: u32 = WM_NULL;

pub struct Executor {
    _not_send: PhantomData<*const ()>,
}

impl Executor {
    pub fn run(f: impl FnOnce(Spawner)) {
        thread_local!(static EXECUTOR_RUNNING: Cell<bool> = const { Cell::new(false) });
        if EXECUTOR_RUNNING.replace(true) {
            panic!("another winmsg-executor is running on the same thread");
        }

        // Callback for the user to spawn tasks.
        f(Spawner::new());

        // Run the windows message loop.
        let mut msg = MaybeUninit::uninit();
        loop {
            let (ret, msg) = unsafe { (GetMessageA(msg.as_mut_ptr(), 0, 0, 0), msg.assume_init()) };
            match ret {
                1 => unsafe {
                    TranslateMessage(&msg);
                    DispatchMessageA(&msg);
                },
                0 => break,
                _ => unreachable!(),
            }
        }

        EXECUTOR_RUNNING.set(false);
    }

    pub fn block_on(future: impl Future<Output = ()> + 'static) {
        Self::run(|spawner| spawner.spawn(future))
    }
}

struct QuitMessageLoopOnDrop;

impl Drop for QuitMessageLoopOnDrop {
    fn drop(&mut self) {
        unsafe { PostQuitMessage(0) };
    }
}

struct TaskState {
    future: Pin<Box<dyn Future<Output = ()>>>,
    _keep_msg_loop_alive: Rc<QuitMessageLoopOnDrop>,
}

impl WindowContext for TaskState {
    fn wndproc(
        &mut self,
        hwnd: HWND,
        msg: u32,
        _wparam: WPARAM,
        _lparam: LPARAM,
    ) -> Option<LRESULT> {
        if msg == MSG_ID_WAKE {
            // Poll the tasks future
            if self
                .future
                .as_mut()
                .poll(&mut Context::from_waker(&waker_for_window(hwnd)))
                .is_ready()
            {
                unsafe { DestroyWindow(hwnd) };
            }
            Some(0)
        } else {
            None
        }
    }
}

pub struct Spawner {
    keep_msg_loop_alive: Rc<QuitMessageLoopOnDrop>,
}

impl Spawner {
    fn new() -> Self {
        Self {
            keep_msg_loop_alive: Rc::new(QuitMessageLoopOnDrop),
        }
    }

    pub fn spawn(&self, future: impl Future<Output = ()> + 'static) {
        let state = TaskState {
            future: Box::pin(future),
            _keep_msg_loop_alive: self.keep_msg_loop_alive.clone(),
        };

        // Create a message only window to run the taks.
        let hwnd = create_window(state);
        debug_assert_ne!(hwnd, 0);

        // Trigger initial poll
        waker_for_window(hwnd).wake();
    }
}

fn waker_for_window(hwnd: HWND) -> Waker {
    unsafe fn wake(hwnd: *const ()) {
        PostMessageA(hwnd as HWND, MSG_ID_WAKE, 0, 0);
    }
    static VTABLE: RawWakerVTable =
        RawWakerVTable::new(|p| RawWaker::new(p, &VTABLE), wake, wake, |_| ());
    unsafe { Waker::from_raw(RawWaker::new(hwnd as *const (), &VTABLE)) }
}
