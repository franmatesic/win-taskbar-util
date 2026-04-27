use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::{self, Command};
use std::thread;
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use windows::core::{w, BOOL};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Threading::{
    OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABM_GETSTATE, ABM_SETSTATE, ABS_ALWAYSONTOP, ABS_AUTOHIDE, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GetClassNameW, IsWindowVisible, ShowWindow, SW_HIDE, SW_SHOW,
};

const ROOT_COMMAND: &str = "wintaskbarutil";
const STARTUP_ENTRY_NAME: &str = "Win Taskbar Util";
const VERSION: &str = env!("CARGO_PKG_VERSION");

const PID_FILE_NAME: &str = "daemon.pid";
const STATE_FILE_NAME: &str = "appbar_state.txt";

const WATCHDOG_INTERVAL: Duration = Duration::from_millis(250);

const DETACHED_PROCESS: u32 = 0x00000008;
const CREATE_NO_WINDOW: u32 = 0x08000000;

fn main() {
    if let Err(err) = run() {
        eprintln!("Error: {err}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<OsString> = env::args_os().skip(1).collect();

    if args.is_empty() {
        print_help();
        return Ok(());
    }

    let command = args[0].to_string_lossy().to_string();

    match command.as_str() {
        "hide" => cmd_hide(),
        "show" => cmd_show(),
        "status" => cmd_status(),
        "autostart" => cmd_autostart(&args[1..]),
        "version" | "--version" | "-v" => {
            println!("{ROOT_COMMAND} {VERSION}");
            Ok(())
        }
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        "--daemon" => daemon_main(),
        other => Err(format!("Unknown command: {other}. Use `{ROOT_COMMAND} help`.").into()),
    }
}

fn cmd_hide() -> Result<(), Box<dyn Error>> {
    if let Some(pid) = read_pid_file() {
        if process_is_running(pid) {
            println!("Taskbar is already hidden. Daemon PID: {pid}");
            return Ok(());
        }

        remove_pid_file();
    }

    let exe = env::current_exe()?;

    let child = Command::new(exe)
        .arg("--daemon")
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()?;

    write_pid_file(child.id())?;

    println!("Taskbar hidden. Daemon PID: {}", child.id());

    Ok(())
}

fn cmd_show() -> Result<(), Box<dyn Error>> {
    if let Some(pid) = read_pid_file() {
        if process_is_running(pid) {
            terminate_process(pid)?;
            println!("Stopped daemon. PID: {pid}");
        } else {
            println!("Daemon PID file existed, but process was not running.");
        }

        remove_pid_file();
    }

    let old_state = read_saved_appbar_state().unwrap_or(ABS_ALWAYSONTOP as u32);

    restore_taskbar(old_state);
    remove_state_file();

    println!("Taskbar shown.");

    Ok(())
}

fn cmd_status() -> Result<(), Box<dyn Error>> {
    let pid = read_pid_file();
    let daemon_running = pid.map(process_is_running).unwrap_or(false);
    let taskbar_hidden = taskbar_appears_hidden();
    let autostart = autostart_is_enabled()?;

    println!("Status:");
    println!("  taskbar hidden: {}", yes_no(taskbar_hidden));
    println!("  daemon running: {}", yes_no(daemon_running));

    if let Some(pid) = pid {
        println!("  daemon pid: {pid}");
    } else {
        println!("  daemon pid: none");
    }

    println!("  autostart: {}", yes_no(autostart));

    Ok(())
}

fn cmd_autostart(args: &[OsString]) -> Result<(), Box<dyn Error>> {
    if args.is_empty() {
        println!("Usage:");
        println!("  {ROOT_COMMAND} autostart on");
        println!("  {ROOT_COMMAND} autostart off");
        println!("  {ROOT_COMMAND} autostart status");
        return Ok(());
    }

    let subcommand = args[0].to_string_lossy().to_string();

    match subcommand.as_str() {
        "on" | "enable" => {
            enable_autostart()?;
            println!("Autostart enabled.");
            Ok(())
        }
        "off" | "disable" => {
            disable_autostart()?;
            println!("Autostart disabled.");
            Ok(())
        }
        "status" => {
            println!("Autostart: {}", yes_no(autostart_is_enabled()?));
            Ok(())
        }
        other => Err(format!("Unknown autostart command: {other}").into()),
    }
}

fn daemon_main() -> Result<(), Box<dyn Error>> {
    let old_state = read_saved_appbar_state().unwrap_or_else(get_appbar_state);

    write_state_file(old_state)?;
    write_pid_file(process::id())?;

    loop {
        suppress_taskbar();
        thread::sleep(WATCHDOG_INTERVAL);
    }
}

fn suppress_taskbar() {
    set_appbar_state((ABS_AUTOHIDE | ABS_ALWAYSONTOP) as u32);
    hide_all_taskbars();
}

fn restore_taskbar(old_state: u32) {
    set_appbar_state(old_state);
    show_all_taskbars();
}

fn get_appbar_state() -> u32 {
    unsafe {
        let mut appbar = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            ..Default::default()
        };

        SHAppBarMessage(ABM_GETSTATE, &mut appbar) as u32
    }
}

fn set_appbar_state(state: u32) {
    unsafe {
        let mut appbar = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            ..Default::default()
        };

        appbar.lParam = LPARAM(state as isize);

        let _ = SHAppBarMessage(ABM_SETSTATE, &mut appbar);
    }
}

fn hide_all_taskbars() {
    for hwnd in find_taskbar_windows() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

fn show_all_taskbars() {
    for hwnd in find_taskbar_windows() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
    }
}

fn taskbar_appears_hidden() -> bool {
    let taskbars = find_taskbar_windows();

    if taskbars.is_empty() {
        return false;
    }

    taskbars
        .iter()
        .all(|hwnd| unsafe { !IsWindowVisible(*hwnd).as_bool() })
}

fn find_taskbar_windows() -> Vec<HWND> {
    let mut hwnds = Vec::<HWND>::new();

    unsafe {
        if let Ok(hwnd) = FindWindowW(w!("Shell_TrayWnd"), None) {
            if !hwnd.0.is_null() {
                hwnds.push(hwnd);
            }
        }

        let _ = EnumWindows(
            Some(enum_windows_for_taskbars),
            LPARAM(&mut hwnds as *mut _ as isize),
        );
    }

    dedupe_hwnds(&mut hwnds);

    hwnds
}

unsafe extern "system" fn enum_windows_for_taskbars(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let hwnds = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    let class_name = unsafe { get_window_class_name(hwnd) };

    if class_name == "Shell_SecondaryTrayWnd" {
        hwnds.push(hwnd);
    }

    true.into()
}

unsafe fn get_window_class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];

    let len = unsafe { GetClassNameW(hwnd, &mut buffer) };

    if len == 0 {
        return String::new();
    }

    String::from_utf16_lossy(&buffer[..len as usize])
}

fn dedupe_hwnds(hwnds: &mut Vec<HWND>) {
    let mut seen = Vec::<isize>::new();

    hwnds.retain(|hwnd| {
        let value = hwnd.0 as isize;

        if seen.contains(&value) {
            false
        } else {
            seen.push(value);
            true
        }
    });
}

fn process_is_running(pid: u32) -> bool {
    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => {
                let _ = CloseHandle(handle);
                true
            }
            Err(_) => false,
        }
    }
}

fn terminate_process(pid: u32) -> Result<(), Box<dyn Error>> {
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, false, pid)?;

        let result = TerminateProcess(handle, 0);
        let _ = CloseHandle(handle);

        result.ok();
    }

    Ok(())
}

fn app_data_dir() -> PathBuf {
    let base = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);

    base.join(ROOT_COMMAND)
}

fn pid_file_path() -> PathBuf {
    app_data_dir().join(PID_FILE_NAME)
}

fn state_file_path() -> PathBuf {
    app_data_dir().join(STATE_FILE_NAME)
}

fn write_pid_file(pid: u32) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(app_data_dir())?;
    fs::write(pid_file_path(), pid.to_string())?;
    Ok(())
}

fn read_pid_file() -> Option<u32> {
    let text = fs::read_to_string(pid_file_path()).ok()?;
    text.trim().parse::<u32>().ok()
}

fn remove_pid_file() {
    let _ = fs::remove_file(pid_file_path());
}

fn write_state_file(state: u32) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(app_data_dir())?;
    fs::write(state_file_path(), state.to_string())?;
    Ok(())
}

fn read_saved_appbar_state() -> Option<u32> {
    let text = fs::read_to_string(state_file_path()).ok()?;
    text.trim().parse::<u32>().ok()
}

fn remove_state_file() {
    let _ = fs::remove_file(state_file_path());
}

fn autostart_command() -> Result<String, Box<dyn Error>> {
    let exe = env::current_exe()?;
    Ok(format!("\"{}\" hide", exe.display()))
}

fn enable_autostart() -> Result<(), Box<dyn Error>> {
    let value = autostart_command()?;

    let status = Command::new("reg")
        .args([
            "add",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            STARTUP_ENTRY_NAME,
            "/t",
            "REG_SZ",
            "/d",
            &value,
            "/f",
        ])
        .status()?;

    if !status.success() {
        return Err("Failed to enable autostart using reg.exe".into());
    }

    Ok(())
}

fn disable_autostart() -> Result<(), Box<dyn Error>> {
    let status = Command::new("reg")
        .args([
            "delete",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            STARTUP_ENTRY_NAME,
            "/f",
        ])
        .status()?;

    if !status.success() {
        return Ok(());
    }

    Ok(())
}

fn autostart_is_enabled() -> Result<bool, Box<dyn Error>> {
    let output = Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            STARTUP_ENTRY_NAME,
        ])
        .output()?;

    Ok(output.status.success())
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn print_help() {
    println!(
        "\
{ROOT_COMMAND} {VERSION}

Usage:
  {ROOT_COMMAND} hide
  {ROOT_COMMAND} show
  {ROOT_COMMAND} status
  {ROOT_COMMAND} autostart on
  {ROOT_COMMAND} autostart off
  {ROOT_COMMAND} autostart status
  {ROOT_COMMAND} version
  {ROOT_COMMAND} help

Commands:
  hide
      Starts a detached background process that keeps the Windows taskbar hidden.
      This command exits immediately.

  show
      Kills the background process, restores the taskbar, and clears saved state.
      This command exits immediately.

  status
      Prints whether the taskbar appears hidden, whether the daemon is running,
      and whether autostart is enabled.

  autostart on
      Adds this app to Windows Startup Apps for the current user.

  autostart off
      Removes this app from Windows Startup Apps for the current user.

  autostart status
      Prints whether autostart is enabled.

  version
      Prints the app version.

  help
      Prints this help message."
    );
}
