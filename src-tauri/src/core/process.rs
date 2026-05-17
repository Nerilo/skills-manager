use std::process::Command;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[cfg(target_os = "windows")]
pub(crate) fn windows_gui_subprocess_creation_flags() -> u32 {
    CREATE_NO_WINDOW
}

pub(crate) fn hide_console_window(command: &mut Command) -> &mut Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_gui_subprocess_creation_flags());
    }

    command
}

pub(crate) fn wsl_command() -> Command {
    let mut command = Command::new("wsl.exe");
    hide_console_window(&mut command);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    #[test]
    fn gui_subprocesses_do_not_create_console_windows_on_windows() {
        assert_eq!(windows_gui_subprocess_creation_flags(), 0x08000000);
    }

    #[test]
    fn wsl_commands_are_built_through_the_gui_safe_builder() {
        let _command: Command = wsl_command();
    }
}
