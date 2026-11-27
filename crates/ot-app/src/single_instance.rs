//! One dock per user session: a named mutex in the session namespace, as OpenTaskbar's
//! `Local\OpenTaskbar.Singleton`. A second instance must exit before touching the taskbar or recovery journals.

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    const ERROR_ALREADY_EXISTS: u32 = 183;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateMutexW(
            attributes: *const c_void,
            initial_owner: i32,
            name: *const u16,
        ) -> *mut c_void;
        fn GetLastError() -> u32;
        fn ReleaseMutex(handle: *mut c_void) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    pub struct SingleInstance(*mut c_void);

    impl SingleInstance {
        /// Takes the session-wide lock, or returns `None` when another instance holds it.
        pub fn acquire(name: &str) -> Option<Self> {
            let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            // SAFETY: `wide` is a null-terminated UTF-16 string that outlives the call.
            let handle = unsafe { CreateMutexW(std::ptr::null(), 1, wide.as_ptr()) };
            // SAFETY: reads this thread's last error right after CreateMutexW.
            let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
            if handle.is_null() {
                // The lock cannot be created at all (unusual); refuse rather than risk two docks.
                return None;
            }
            if already_exists {
                // SAFETY: closing the handle CreateMutexW just returned.
                unsafe { CloseHandle(handle) };
                return None;
            }
            Some(Self(handle))
        }
    }

    impl Drop for SingleInstance {
        fn drop(&mut self) {
            // SAFETY: this instance owns the mutex (created with initial ownership) and its handle.
            unsafe {
                ReleaseMutex(self.0);
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub struct SingleInstance;
    impl SingleInstance {
        pub fn acquire(_name: &str) -> Option<Self> {
            Some(Self)
        }
    }
}

pub use imp::SingleInstance;

pub const NAME: &str = r"Local\OpenTaskBar-Rust.Singleton";
pub const DEV_NAME: &str = r"Local\OpenTaskBar-Rust.Dev";
