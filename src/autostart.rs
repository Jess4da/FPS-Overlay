//! "Start with Windows": a Task Scheduler logon task running the exe at highest
//! privileges, so login starts it elevated without a UAC prompt. The task itself
//! is the state; nothing is stored in config.ini.

use std::os::windows::process::CommandExt;
use std::process::Command;

const TASK: &str = "FPS Overlay";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn schtasks(args: &[&str]) -> Result<(), String> {
    let out = Command::new("schtasks.exe")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// True if the logon task exists. ponytail: doesn't check it points at this exe;
/// re-enabling after moving the exe rewrites it.
pub fn enabled() -> bool {
    schtasks(&["/Query", "/TN", TASK]).is_ok()
}

pub fn set(on: bool) -> Result<(), String> {
    if !on {
        return schtasks(&["/Delete", "/TN", TASK, "/F"]);
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let user = format!(
        "{}\\{}",
        std::env::var("USERDOMAIN").unwrap_or_default(),
        std::env::var("USERNAME").map_err(|e| e.to_string())?
    );
    let dir = exe.parent().map(|d| d.to_string_lossy().into_owned());
    let xml = task_xml(&exe.to_string_lossy(), dir.as_deref().unwrap_or(""), &user);
    // schtasks reads UTF-16 LE with BOM, matching the XML declaration.
    let bytes: Vec<u8> = std::iter::once(0xFEFF)
        .chain(xml.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect();
    let path = std::env::temp_dir().join("fps-overlay-task.xml");
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let r = schtasks(&[
        "/Create",
        "/TN",
        TASK,
        "/XML",
        &path.to_string_lossy(),
        "/F",
    ]);
    let _ = std::fs::remove_file(&path);
    r
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// XML instead of `/SC ONLOGON` flags: the flag defaults stop the task on battery
/// and after 72 h, which would kill the overlay. Mirrors a known-good logon task:
/// schema 1.4, `StartWhenAvailable` (retry a missed logon start), never stop on
/// idle end, unified engine, and a working directory next to the exe.
fn task_xml(exe: &str, dir: &str, user: &str) -> String {
    let (exe, dir, user) = (esc(exe), esc(dir), esc(user));
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers><LogonTrigger><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <AllowHardTerminate>false</AllowHardTerminate>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <StartWhenAvailable>true</StartWhenAvailable>
    <IdleSettings><StopOnIdleEnd>false</StopOnIdleEnd><RestartOnIdle>false</RestartOnIdle></IdleSettings>
    <UseUnifiedSchedulingEngine>true</UseUnifiedSchedulingEngine>
  </Settings>
  <Actions Context="Author"><Exec><Command>{exe}</Command><WorkingDirectory>{dir}</WorkingDirectory></Exec></Actions>
</Task>
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_xml_escapes_markup_in_path_and_user() {
        let x = task_xml(
            r"C:\A&B <x>\fps-overlay.exe",
            r"C:\A&B <x>",
            "PC\\Tom&Jerry",
        );
        assert!(x.contains(r"<Command>C:\A&amp;B &lt;x&gt;\fps-overlay.exe</Command>"));
        assert!(x.contains(r"<WorkingDirectory>C:\A&amp;B &lt;x&gt;</WorkingDirectory>"));
        assert!(x.contains("<UserId>PC\\Tom&amp;Jerry</UserId>"));
        assert!(!x.contains("A&B"));
    }
}
