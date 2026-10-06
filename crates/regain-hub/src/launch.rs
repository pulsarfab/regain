//! Shared hosts outlive individual launchers. Never inherit a frontend's capture
//! pipes or put this process in a kill-on-frontend-exit worker job.
use std::{io, path::Path};

pub async fn spawn_host(executable: &Path, config: &Path, workers: &Path) -> io::Result<u32> {
    if !executable.is_absolute()
        || !executable.is_file()
        || !config.is_absolute()
        || !config.is_file()
        || !workers.is_absolute()
        || !workers.is_dir()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Host launch requires existing absolute paths",
        ));
    }
    #[cfg(windows)]
    {
        windows::spawn(executable, config, workers)
    }
    #[cfg(unix)]
    {
        use std::{os::unix::process::CommandExt, process::Stdio};
        let mut command = tokio::process::Command::new(executable);
        command
            .arg("--hub-host")
            .arg("--hub-config")
            .arg(config)
            .arg("--workers")
            .arg(workers)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(false);
        command.as_std_mut().process_group(0);
        let mut child = command.spawn()?;
        let id = child
            .id()
            .ok_or_else(|| io::Error::other("Host exited before launch completed"))?;
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        Ok(id)
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{ffi::OsStr, os::windows::ffi::OsStrExt, ptr::null};
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, PROCESS_INFORMATION,
            STARTUPINFOW,
        },
    };

    fn argument(value: &OsStr) -> io::Result<Vec<u16>> {
        // Windows CRT argv quoting, including embedded quotes and terminal
        // backslashes. No shell, interpolation, or command-string execution.
        let mut encoded = vec![b'"' as u16];
        let mut slashes = 0;
        for c in value.encode_wide() {
            if c == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "NUL in host argument",
                ));
            }
            if c == b'\\' as u16 {
                slashes += 1;
                continue;
            }
            encoded.extend(std::iter::repeat_n(
                b'\\' as u16,
                if c == b'"' as u16 {
                    slashes * 2 + 1
                } else {
                    slashes
                },
            ));
            slashes = 0;
            encoded.push(c);
        }
        encoded.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
        encoded.push(b'"' as u16);
        Ok(encoded)
    }
    pub fn spawn(executable: &Path, config: &Path, workers: &Path) -> io::Result<u32> {
        let mut application: Vec<u16> = executable.as_os_str().encode_wide().collect();
        if application.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NUL in host executable",
            ));
        }
        application.push(0);
        let mut command_line = Vec::new();
        for value in [
            executable.as_os_str(),
            OsStr::new("--hub-host"),
            OsStr::new("--hub-config"),
            config.as_os_str(),
            OsStr::new("--workers"),
            workers.as_os_str(),
        ] {
            if !command_line.is_empty() {
                command_line.push(b' ' as u16);
            }
            command_line.extend(argument(value)?);
        }
        command_line.push(0);
        if command_line.len() > 32767 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Host command line exceeds the Windows limit",
            ));
        }
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            ..unsafe { std::mem::zeroed() }
        };
        let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // bInheritHandles=false is essential. Stdio::null alone still permits
        // other inheritable pipe handles from the launcher to leak into a child.
        if unsafe {
            CreateProcessW(
                application.as_ptr(),
                command_line.as_mut_ptr(),
                null(),
                null(),
                0,
                CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
                null(),
                null(),
                &startup,
                &mut process,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        unsafe {
            CloseHandle(process.hThread);
            CloseHandle(process.hProcess);
        }
        Ok(process.dwProcessId)
    }

    #[test]
    fn arguments_preserve_spaces_quotes_backslashes_and_unicode_without_a_shell() {
        for (value, expected) in [
            ("", "\"\""),
            ("two words", "\"two words\""),
            ("a\"b", "\"a\\\"b\""),
            ("C:\\end\\", "\"C:\\end\\\\\""),
            ("a\\\"b", "\"a\\\\\\\"b\""),
            ("\u{03bb} scope", "\"\u{03bb} scope\""),
        ] {
            assert_eq!(
                String::from_utf16(&argument(OsStr::new(value)).unwrap()).unwrap(),
                expected
            );
        }
        assert!(argument(OsStr::new("bad\0argument")).is_err());
    }
}
