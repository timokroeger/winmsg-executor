use std::{
    cell::Cell,
    ffi::CStr,
    future::Future,
    marker::PhantomData,
    mem::MaybeUninit,
    pin::Pin,
    ptr,
    rc::Rc,
    task::{Context, RawWaker, RawWakerVTable, Waker},
};

use windows_sys::Win32::{Foundation::*, UI::WindowsAndMessaging::*};

const CLASS_NAME: &'static CStr = c"winmsg-executor";
const MSG_ID_WAKE: u32 = WM_NULL;

thread_local! {
    static EXECUTOR_RUNNING: Cell<bool> = const { Cell::new(false) };
}

// Taken from:
// https://github.com/rust-windowing/winit/blob/v0.30.0/src/platform_impl/windows/util.rs#L140
pub fn get_instance_handle() -> HINSTANCE {
    // Gets the instance handle by taking the address of the
    // pseudo-variable created by the microsoft linker:
    // https://devblogs.microsoft.com/oldnewthing/20041025-00/?p=37483

    // This is preferred over GetModuleHandle(NULL) because it also works in DLLs:
    // https://stackoverflow.com/questions/21718027/getmodulehandlenull-vs-hinstance

    extern "C" {
        static __ImageBase: u8;
    }
    unsafe { &__ImageBase as *const _ as _ }
}

fn register_class() {
    let mut wnd_class: WNDCLASSA = unsafe { std::mem::zeroed() };
    wnd_class.lpfnWndProc = Some(wndproc);
    wnd_class.hInstance = get_instance_handle();
    wnd_class.lpszClassName = CLASS_NAME.as_ptr().cast();
    unsafe { RegisterClassA(&wnd_class) };
}

fn unregister_class() {
    unsafe { UnregisterClassA(CLASS_NAME.as_ptr().cast(), get_instance_handle()) };
}

pub struct Executor {
    _not_send: PhantomData<*const ()>,
}

impl Executor {
    pub fn run(f: impl FnOnce(Spawner)) {
        if EXECUTOR_RUNNING.replace(true) {
            panic!("another winmsg-executor is running on the same thread");
        }

        // When running multiple executor threads the `register_class()` call
        // only succeeds for the first thread, afther which the class exists
        // and can be reused by the other executor threads.
        register_class();

        // Callback for the user to stawn tasks.
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

        // In a multi thread scenario the unregistration fails as long as
        // windows using this class exist. Only after exiting from the last
        // executor thread the class will actually be unregistered.
        unregister_class();

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

impl TaskState {
    fn from_hwnd(hwnd: HWND) -> &'static mut Self {
        unsafe {
            let state_ptr = GetWindowLongPtrA(hwnd, GWLP_USERDATA) as *mut TaskState;
            state_ptr.as_mut().unwrap()
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
        let state_ptr = Box::into_raw(Box::new(state));

        // Create a message only window to run the taks.
        let hwnd = unsafe {
            CreateWindowExA(
                0,
                CLASS_NAME.as_ptr().cast(),
                ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                0,
                get_instance_handle(),
                state_ptr.cast(),
            )
        };
        assert_ne!(hwnd, 0);

        // Trigger initial poll
        waker_for_window(hwnd).wake();
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // println!(
    //     "WND  hwnd={:p} msg={:06}, wparam={:06}, lparam={:06}",
    //     hwnd as *const (), msg, wparam, lparam
    // );

    if msg == WM_GETMINMAXINFO {
        // This is the very first message received by this function when calling
        // `CreateWindowExA()`. The user data pointer has not been set yet.
        // Run the default handler and return early.
        return DefWindowProcA(hwnd, msg, wparam, lparam);
    }

    if msg == WM_NCCREATE {
        // Attach user data to the window so it can be accessed from this
        // callback function when receiving other messages.
        // This must be done here because the WM_NCCREATE (which is the second
        // message after `WM_GETMINMAXINFO`) and other message (e.g WM_CREATE)
        // are dispatched to this callback before `CreateWindowEx()` returns.
        // https://devblogs.microsoft.com/oldnewthing/20191014-00/?p=102992
        let create_params = lparam as *const CREATESTRUCTA;
        SetWindowLongPtrA(
            hwnd,
            GWLP_USERDATA,
            (*create_params).lpCreateParams as isize,
        );
        return 1; // Continue with window creation
    }

    let state = TaskState::from_hwnd(hwnd);
    match msg {
        MSG_ID_WAKE => {
            // Poll the taks future
            if state
                .future
                .as_mut()
                .poll(&mut Context::from_waker(&waker_for_window(hwnd)))
                .is_ready()
            {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_NCDESTROY => {
            // This is the very last message received by this function before
            // the windows is destroyed. Deallocate the task state.
            drop(Box::from_raw(state));
            0
        }
        _ => DefWindowProcA(hwnd, msg, wparam, lparam),
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
