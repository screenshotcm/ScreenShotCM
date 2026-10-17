//! The agent: a hidden window that owns the tray icon and the hotkeys and
//! runs every command on the UI thread. It is never shown; it exists so
//! Windows has somewhere to deliver `WM_HOTKEY`, the tray's callbacks and the
//! taskbar's restart broadcast (a message-only window would miss the last).

use std::cell::RefCell;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::UI::Shell::{NIN_SELECT, NINF_KEY};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    MSG, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, TranslateMessage,
    WINDOW_EX_STYLE, WM_CONTEXTMENU, WM_COPYDATA, WM_DESTROY, WM_HOTKEY, WNDCLASSEXW,
    WS_OVERLAPPED,
};
use windows::core::{PCWSTR, w};

use crate::commands::{self, Command, Request};
use crate::settings::Settings;
use crate::tray::{Tray, WM_TRAY};
use crate::{COPYDATA_REQUEST, autostart, hotkeys, log};

pub const CLASS: PCWSTR = w!("ScreenShotCM.Agent");

pub struct Agent {
    pub hwnd: HWND,
    pub settings: Settings,
    pub tray: Tray,
    taskbar_created: u32,
    started: Instant,
}

/// How long after the agent starts a plain relaunch is taken for the
/// sign-in starting it twice (the Run key and Windows' "restart apps", or
/// the MSI and the Store copy side by side) rather than for the user
/// opening it from Start: one showed the settings at every sign-in.
fn startup_grace() -> Duration {
    // A debug build takes `SCM_STARTUP_GRACE_SECS`, so the gate can be
    // tested without waiting it out.
    if cfg!(debug_assertions)
        && let Some(secs) = std::env::var_os("SCM_STARTUP_GRACE_SECS")
        && let Some(secs) = secs.to_str().and_then(|s| s.parse().ok())
    {
        return Duration::from_secs(secs);
    }
    Duration::from_secs(120)
}

thread_local! {
    pub static AGENT: RefCell<Option<Agent>> = const { RefCell::new(None) };
}

/// Runs `f` on the agent. Never call it from inside another `with` — a
/// command that opens a window re-enters the window procedure, so the borrow
/// is kept as short as one statement.
pub fn with<R>(f: impl FnOnce(&mut Agent) -> R) -> Option<R> {
    AGENT.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

pub fn settings() -> Settings {
    with(|a| a.settings.clone()).unwrap_or_default()
}

pub fn hwnd() -> HWND {
    with(|a| a.hwnd).unwrap_or_default()
}

pub fn notify(title: &str, text: &str, warning: bool) {
    with(|a| a.tray.notify(title, text, warning));
}

pub fn run(first: Request) -> i32 {
    unsafe {
        // OLE, not just COM: dragging a capture out to another program
        // needs it on the UI thread.
        let _ = OleInitialize(None);
        // A drag's file left by a crash: nothing unsaved is kept.
        crate::dragdrop::clear_staging();
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&class);
        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("ScreenShotCM"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                log::error(&format!("agent window: {e}"));
                return 1;
            }
        };

        let settings = Settings::load();
        crate::i18n::apply(settings.language);
        crate::theme::apply(settings.theme);
        // First run: write the file out, so the user has something to edit.
        // The welcome below hangs on that write: where it fails (a profile
        // the agent cannot write to) every start would be the first, and
        // the settings would open at every sign-in.
        let first_run = !crate::settings::file().exists();
        let welcome = first_run
            && first == Request::None
            && match settings.save() {
                Ok(()) => true,
                Err(e) => {
                    log::error(&format!("settings save: {e}"));
                    false
                }
            };
        let tray = Tray::new(hwnd);
        AGENT.with(|a| {
            *a.borrow_mut() = Some(Agent {
                hwnd,
                settings: settings.clone(),
                tray,
                taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
                started: Instant::now(),
            })
        });
        register_hotkeys();
        // The very first start shows the settings as a welcome: where the
        // shortcuts are and what happens after a capture. Starts with
        // Windows afterwards stay silent in the tray.
        handle(first);
        if welcome {
            dispatch(Command::OpenSettings);
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        hotkeys::unregister_all(hwnd);
        // Dropping the tray removes the icon; a leftover icon lingers until
        // the user hovers over it.
        AGENT.with(|a| a.borrow_mut().take());
    }
    0
}

/// Settings saved by the settings window take effect at once.
pub fn replace_settings(settings: Settings) {
    crate::i18n::apply(settings.language);
    crate::theme::apply(settings.theme);
    with(|a| a.settings = settings);
}

/// After the settings window, which releases the hotkeys while it records
/// new ones.
pub fn reregister_hotkeys() {
    register_hotkeys();
}

fn register_hotkeys() {
    let Some((hwnd, settings)) = with(|a| (a.hwnd, a.settings.clone())) else {
        return;
    };
    let problems = hotkeys::register_all(hwnd, &settings);
    if !problems.is_empty() {
        let text = problems.join("\n");
        log::error(&format!("hotkeys: {text}"));
        notify(
            crate::i18n::tr(
                "Algunos atajos no están activos",
                "Some shortcuts are not active",
            ),
            &text,
            true,
        );
    }
}

pub fn handle(request: Request) {
    match request {
        Request::None => {}
        // Opened again from Start or a shortcut with the agent already in
        // the tray: show the settings, so opening it always shows something.
        // Not while signing in, where a second launch is the system's doing.
        Request::Relaunch => {
            let young = with(|a| a.started.elapsed() < startup_grace()).unwrap_or(false);
            if young {
                log::info("relaunch while starting: ignored");
            } else {
                dispatch(Command::OpenSettings);
            }
        }
        Request::Run(c) => dispatch(c),
        Request::Annotate(path) => match crate::image::load(&path) {
            // A file opened from outside is edited as a copy: «Listo» puts
            // the result in the history, never over the user's own file.
            Ok(img) => crate::editor::open(img, None),
            Err(e) => notify(
                crate::i18n::tr("No se pudo abrir la imagen", "Could not open the image"),
                &format!("{}: {e}", path.display()),
                true,
            ),
        },
        Request::Pin(path) => match crate::image::load(&path) {
            Ok(img) => crate::pin::open(img, None),
            Err(e) => notify(
                crate::i18n::tr("No se pudo abrir la imagen", "Could not open the image"),
                &format!("{}: {e}", path.display()),
                true,
            ),
        },
    }
}

pub fn dispatch(command: Command) {
    log::info(&format!("command {command:?}"));
    match command {
        Command::OpenSaveFolder => {
            let dir = settings().save_dir;
            let _ = std::fs::create_dir_all(&dir);
            open_path(&dir.to_string_lossy());
        }
        Command::OpenSettings => crate::settingswin::open(),
        Command::ReloadSettings => {
            let fresh = Settings::load();
            replace_settings(fresh);
            // With the settings open the hotkeys stay released (a field may
            // be recording Print Screen); the window registers them as it
            // closes.
            if !crate::settingswin::is_open() {
                register_hotkeys();
            }
            notify(
                crate::i18n::tr("Ajustes recargados", "Settings reloaded"),
                crate::i18n::tr(
                    "Los atajos y opciones nuevos ya están activos.",
                    "The new shortcuts and options are active.",
                ),
                false,
            );
        }
        Command::ToggleAutostart => {
            if let Err(e) = autostart::set(!autostart::is_enabled()) {
                notify(
                    crate::i18n::tr(
                        "No se pudo cambiar el inicio con Windows",
                        "Could not change starting with Windows",
                    ),
                    &e,
                    true,
                );
            }
        }
        Command::OpenHistory => crate::historywin::open(),
        Command::OpenFromClipboard => match crate::clipboard::get_image(hwnd()) {
            Some(img) => crate::editor::open(img, None),
            None => notify(
                crate::i18n::tr(
                    "El portapapeles no tiene una imagen",
                    "The clipboard holds no image",
                ),
                crate::i18n::tr(
                    "Copia una imagen y vuelve a intentarlo.",
                    "Copy an image and try again.",
                ),
                false,
            ),
        },
        Command::OpenFile => {
            if let Some(path) = pick_image() {
                handle(Request::Annotate(path));
            }
        }
        Command::RestoreRecentlyClosed => crate::quick::restore_last(),
        Command::UnlockPins => crate::pin::unlock_all(),
        Command::ClosePins => crate::pin::close_all(),
        Command::Quit => unsafe {
            let _ = DestroyWindow(hwnd());
        },
        Command::CaptureArea
        | Command::CaptureWindow
        | Command::CaptureFullscreen
        | Command::CapturePreviousArea
        | Command::CaptureScrolling
        | Command::SelfTimer => crate::flow::run(command),
    }
}

/// The common open dialog, for images Windows can decode.
fn pick_image() -> Option<std::path::PathBuf> {
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OPENFILENAMEW,
    };
    let mut buffer = [0u16; 1024];
    let (images, all) = (
        crate::i18n::tr("Imágenes", "Images"),
        crate::i18n::tr("Todos los archivos", "All files"),
    );
    let filter: Vec<u16> = format!(
        "{images}\0*.png;*.jpg;*.jpeg;*.bmp;*.gif;*.tif;*.tiff;*.webp;*.heic\0{all}\0*.*\0\0"
    )
    .encode_utf16()
    .collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd(),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: windows::core::PWSTR(buffer.as_mut_ptr()),
        nMaxFile: buffer.len() as u32,
        Flags: OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    if !unsafe { GetOpenFileNameW(&mut ofn) }.as_bool() {
        return None;
    }
    let len = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    Some(std::path::PathBuf::from(String::from_utf16_lossy(
        &buffer[..len],
    )))
}

/// Opens a file or folder with whatever the user has associated, through a
/// separate Explorer. `ShellExecute` in our own process loaded 51 shell
/// modules and left six threads behind for the agent's whole life.
pub fn open_path(path: &str) {
    if let Err(e) = std::process::Command::new("explorer.exe").arg(path).spawn() {
        log::error(&format!("open {path}: {e}"));
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        windows::Win32::UI::WindowsAndMessaging::WM_TIMER
            if wparam.0 == crate::clipboard::RETRY_TIMER =>
        {
            crate::clipboard::retry();
            LRESULT(0)
        }
        WM_HOTKEY => {
            if let Some(c) = hotkeys::command_for(wparam.0 as i32) {
                dispatch(c);
            }
            LRESULT(0)
        }
        WM_TRAY => {
            // NOTIFYICON_VERSION_4: the event is the low word of lParam.
            let event = (lparam.0 & 0xFFFF) as u32;
            match event {
                e if e == NIN_SELECT || e == NIN_SELECT | NINF_KEY => {
                    dispatch(Command::CaptureArea)
                }
                WM_CONTEXTMENU => {
                    let mut at = POINT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut at);
                    }
                    let s = settings();
                    let chosen = crate::tray::menu(hwnd, at, &s);
                    if let Some(c) = chosen {
                        dispatch(c);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_COPYDATA => {
            let data = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
            if data.dwData == COPYDATA_REQUEST && !data.lpData.is_null() && data.cbData <= 64 * 1024
            {
                let bytes = unsafe {
                    std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize)
                };
                let request = commands::decode(&String::from_utf8_lossy(bytes));
                // Answer the sender first: it waits on this message, and a
                // capture would hold it for as long as the overlay is open.
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::ReplyMessage(LRESULT(1));
                }
                handle(request);
            }
            LRESULT(1)
        }
        WM_DESTROY => {
            // Quit in the middle of a scrolling capture: give the pointer
            // back and close its windows.
            crate::scroll::cancel();
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => {
            let restarted = with(|a| a.taskbar_created).unwrap_or(0);
            if restarted != 0 && msg == restarted {
                with(|a| a.tray.add());
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}
