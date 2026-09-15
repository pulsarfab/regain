#[cfg(not(windows))]
pub struct ProcessGuard;
#[cfg(not(windows))]
impl ProcessGuard {
    pub fn attach(_: u32) -> anyhow::Result<Self> {
        Ok(Self)
    }
}

#[cfg(windows)]
mod windows {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{
            JobObjects::*,
            Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
        },
    };
    pub struct ProcessGuard(HANDLE);
    // The owned job handle is closed once, independently of the calling thread.
    unsafe impl Send for ProcessGuard {}
    unsafe impl Sync for ProcessGuard {}
    impl ProcessGuard {
        pub fn attach(pid: u32) -> anyhow::Result<Self> {
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return Err(std::io::Error::last_os_error().into());
                }
                let guard = Self(job);
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const _,
                    std::mem::size_of_val(&limits) as u32,
                ) == 0
                {
                    return Err(std::io::Error::last_os_error().into());
                }
                let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
                if process.is_null() {
                    return Err(std::io::Error::last_os_error().into());
                }
                let result = AssignProcessToJobObject(job, process);
                let error = std::io::Error::last_os_error();
                CloseHandle(process);
                if result == 0 {
                    return Err(error.into());
                }
                Ok(guard)
            }
        }
    }
    impl Drop for ProcessGuard {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
#[cfg(windows)]
pub use windows::ProcessGuard;
