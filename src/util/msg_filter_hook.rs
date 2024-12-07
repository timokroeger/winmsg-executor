use std::{
    cell::Cell,
    marker::PhantomData,
    panic::{self, AssertUnwindSafe},
    ptr,
};
use windows_sys::Win32::{
    Foundation::*, System::Threading::GetCurrentThreadId, UI::WindowsAndMessaging::*,
};

thread_local! {
    static MSG_FILTER_HOOK: Cell<*mut ()> = const { Cell::new(ptr::null_mut()) };
}

pub struct MsgFilterHook<'a, F> {
    handle: HHOOK,
    _lifetime_and_type: PhantomData<&'a F>,
}

impl<'a, F: Fn(&MSG) -> bool + 'a> MsgFilterHook<'a, F> {
    /// # Safety
    ///
    /// This function is safe as long as the returned handle is not leaked
    /// or if the provided handler closure is `'static`.
    pub unsafe fn register(handler: F) -> Self {
        assert!(MSG_FILTER_HOOK.get().is_null());

        MSG_FILTER_HOOK.set(Box::into_raw(Box::new(handler)) as *mut ());

        let handle = SetWindowsHookExA(
            WH_MSGFILTER,
            Some(hook_proc::<F>),
            ptr::null_mut(),
            GetCurrentThreadId(),
        );
        Self {
            handle,
            _lifetime_and_type: PhantomData,
        }
    }
}

impl<F> Drop for MsgFilterHook<'_, F> {
    fn drop(&mut self) {
        unsafe {
            UnhookWindowsHookEx(self.handle);
            drop(Box::from_raw(
                MSG_FILTER_HOOK.replace(ptr::null_mut()) as *mut F
            ));
        }
    }
}

unsafe extern "system" fn hook_proc<F: Fn(&MSG) -> bool>(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let f = &*(MSG_FILTER_HOOK.get() as *mut F);
    let msg = &*(lparam as *const MSG);
    match panic::catch_unwind(AssertUnwindSafe(|| f(msg))) {
        Ok(true) => 1,
        Ok(false) => CallNextHookEx(ptr::null_mut(), code, wparam, lparam),
        Err(panic_payload) => {
            crate::PANIC_PAYLOAD.set(Some(panic_payload));
            0
        }
    }
}
