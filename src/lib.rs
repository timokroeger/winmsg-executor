mod backend;
pub mod window;

use std::{cell::Cell, future::Future, marker::PhantomData, mem::MaybeUninit, rc::Rc};

use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub struct Executor {
    _not_send: PhantomData<*const ()>,
}

impl Executor {
    pub fn run(f: impl FnOnce(Spawner)) {
        thread_local!(static EXECUTOR_RUNNING: Cell<bool> = const { Cell::new(false) });

        // Prevent calls to `Executor::run()` from tasks.
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
                    if !backend::dispatch(&msg) {
                        TranslateMessage(&msg);
                        DispatchMessageA(&msg);
                    }
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

#[derive(Clone)]
pub struct Spawner {
    msg_loop: Rc<QuitMessageLoopOnDrop>,
}

impl Spawner {
    fn new() -> Self {
        Self {
            msg_loop: Rc::new(QuitMessageLoopOnDrop),
        }
    }

    pub fn spawn(&self, future: impl Future<Output = ()> + 'static) {
        backend::spawn(self.msg_loop.clone(), future);
    }
}
