//! Small subprocess helpers shared by runtime code.

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Hide console windows for child processes spawned by the GUI app on Windows.
///
/// Without this flag, launching console-subsystem programs such as `codex.cmd`,
/// `powershell.exe`, or `taskkill.exe` from a Tauri window can create visible
/// black console windows. Non-Windows platforms do not need an equivalent.
pub fn hide_console_window(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

/// Tokio-process variant of [`hide_console_window`].
pub fn hide_tokio_console_window(cmd: &mut tokio::process::Command) {
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}
