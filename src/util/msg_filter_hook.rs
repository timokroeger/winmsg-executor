use std::{cell::Cell, marker::PhantomData, ptr};
use windows_sys::Win32::{
    Foundation::*, System::Threading::GetCurrentThreadId, UI::WindowsAndMessaging::*,
};

thread_local! {
    static MSG_FILTER_HOOK: Cell<*const ()> = const { Cell::new(ptr::null()) };
}

pub struct MsgFilterHook<'a, F> {
    hhook: HHOOK,
    _hook_proc: Box<F>,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a, F: Fn(&MSG) -> bool + 'a> MsgFilterHook<'a, F> {
    /// # Safety
    ///
    /// This function is safe as long as the returned handle is not leaked
    /// or if the provided handler closure is `'static`.
    pub unsafe fn register(handler: F) -> Self {
        assert!(MSG_FILTER_HOOK.get().is_null());

        let handler = Box::new(handler);
        MSG_FILTER_HOOK.set(&*handler as *const F as *const ());

        unsafe extern "system" fn hook_proc<F: Fn(&MSG) -> bool>(
            code: i32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            if code < 0 {
                return CallNextHookEx(ptr::null_mut(), code, wparam, lparam);
            }

            let f = &*(MSG_FILTER_HOOK.get() as *const F);
            let msg = &*(lparam as *const MSG);

            if f(msg) {
                1
            } else {
                CallNextHookEx(ptr::null_mut(), code, wparam, lparam)
            }
        }

        let hhook = unsafe {
            SetWindowsHookExA(
                WH_MSGFILTER,
                Some(hook_proc::<F>),
                ptr::null_mut(),
                GetCurrentThreadId(),
            )
        };
        Self {
            hhook,
            _hook_proc: handler,
            _lifetime: PhantomData,
        }
    }
}

impl<F> Drop for MsgFilterHook<'_, F> {
    fn drop(&mut self) {
        unsafe { UnhookWindowsHookEx(self.hhook) };
    }
}
