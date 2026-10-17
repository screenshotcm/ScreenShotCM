//! ScreenShotCM: a resident agent in the notification area. One instance per
//! user session; a second launch hands its command to the first and leaves.

#![windows_subsystem = "windows"]
// Pixel loops index several buffers by the same channel; Win32 drawing calls
// take many plain arguments. Both read better as they are.
#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]

mod agent;
mod annot;
mod autostart;
mod background;
mod capture;
mod clipboard;
mod commands;
mod dragdrop;
mod editor;
mod flow;
mod gfx;
mod history;
mod historywin;
mod hotkeys;
mod i18n;
mod icon;
mod icons;
mod image;
mod layered;
mod log;
mod overlay;
mod paths;
mod pin;
mod quick;
mod render;
mod scroll;
mod settings;
mod settingswin;
mod stitch;
mod theme;
mod tray;
mod util;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_COPYDATA,
};
use windows::core::{PCWSTR, w};

use crate::commands::Request;

/// The marker `WM_COPYDATA` carries for a forwarded request.
pub const COPYDATA_REQUEST: usize = 0x5343_4D31; // "SCM1"

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Maintenance verbs, run by hand or by msix/build.ps1 (DEVELOPMENT.md):
    // `--export-icon <file.ico>` rewrites assets/screenshotcm.ico from the
    // drawing in icon.rs, `--export-store-assets <dir>` writes the MSIX tiles.
    // Both encode PNGs with WIC, hence COM.
    let export: Option<Export> = match args.first().map(String::as_str) {
        Some("--export-icon") => Some(export_icon),
        Some("--export-store-assets") => Some(export_tiles),
        _ => None,
    };
    if let Some(export) = export {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_MULTITHREADED,
            );
        }
        let ok = args.get(1).is_some_and(|p| export(p).is_ok());
        std::process::exit(if ok { 0 } else { 1 });
    }

    let request = commands::parse_args(&args);
    // Something was asked and refused (an unknown verb, or a link naming a
    // network path): do nothing at all, rather than whatever a plain launch
    // would do.
    if request == Request::None && !args.is_empty() {
        return;
    }

    // Physical pixels everywhere: a capture must match the screen exactly on
    // every monitor, whatever its scaling.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    // The mutex lives as long as the process; its handle is never closed.
    let _mutex = unsafe { CreateMutexW(None, false, w!("Local\\ScreenShotCM.Agent")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        forward(&request);
        return;
    }
    // "Quit" with no agent running has nothing to stop. Starting one only to
    // stop it would, during an upgrade, open the old executable while the
    // installer replaces it (seen in the 0.1.0 → 0.2.0 install log).
    if request == Request::Run(commands::Command::Quit) {
        return;
    }

    log::init(paths::log_dir());
    log::info(&format!(
        "start {} pid {}",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    ));
    let code = agent::run(request);
    flow::finish_writing();
    // A drag's copy is not the user's to keep.
    dragdrop::clear_staging();
    log::info("exit");
    std::process::exit(code);
}

/// A maintenance verb: writes its output to the path it is given.
type Export = fn(&str) -> Result<(), String>;

fn export_icon(path: &str) -> Result<(), String> {
    std::fs::write(path, icon::ico_file()).map_err(|e| e.to_string())
}

fn export_tiles(dir: &str) -> Result<(), String> {
    icon::export_store_assets(std::path::Path::new(dir))
}

/// Hands the request to the running agent. A plain relaunch (no verb) has
/// nothing to hand over: the agent is already in the tray.
fn forward(request: &Request) {
    // Opened again from Start or a shortcut, with the agent already in the
    // tray: show the settings, so opening it always shows something.
    // A path from a terminal is relative to that terminal's folder, which
    // the agent does not share: send it whole.
    let absolute = |p: &std::path::Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let request = match request {
        Request::None => Request::Relaunch,
        Request::Annotate(p) => Request::Annotate(absolute(p)),
        Request::Pin(p) => Request::Pin(absolute(p)),
        other => other.clone(),
    };
    let text = commands::encode(&request);
    if text.is_empty() {
        return;
    }
    let Ok(hwnd) = (unsafe { FindWindowW(agent::CLASS, PCWSTR::null()) }) else {
        return;
    };
    // This process was started by the user, so it may bring a window
    // forward; the agent may not. Hand it that right, or the editor or
    // the settings it opens appear behind the window the user is in.
    unsafe {
        let mut pid = 0u32;
        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid);
    }
    let data = COPYDATASTRUCT {
        dwData: COPYDATA_REQUEST,
        cbData: text.len() as u32,
        lpData: text.as_ptr() as *mut _,
    };
    unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(&data as *const _ as isize),
            SMTO_ABORTIFHUNG,
            5000,
            None,
        );
    }
}
