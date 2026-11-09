//! Native window + tray for AI Session Vault.
//!
//! Started with no arguments (a double-click, a shortcut, or login), it is the app's launcher: it starts
//! `avault app` in the background, which runs the engine and the local UI server and then runs this
//! program again with the UI's link. With a link it shows the page in the system webview (WebView2 on
//! Windows) and adds a tray icon. Closing the window hides it to the tray (syncing continues); "Quit"
//! exits, which stops the engine.

// A windowed program: no console box when it is started from Explorer or a shortcut.
#![cfg_attr(windows, windows_subsystem = "windows")]

use anyhow::{anyhow, bail, Context, Result};
use std::path::PathBuf;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

const LOGO_PNG: &[u8] = include_bytes!("../../../assets/logo-64.png");

/// The project logo (assets/icon.ico from the repo), decoded to RGBA.
fn logo() -> (Vec<u8>, u32, u32) {
    let mut dec = png::Decoder::new(LOGO_PNG);
    dec.set_transformations(png::Transformations::EXPAND);
    let mut r = dec.read_info().expect("logo png");
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).expect("logo frame");
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.chunks(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        _ => buf.iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
    };
    (rgba, info.width, info.height)
}

fn icon() -> Icon {
    let (px, w, h) = logo();
    Icon::from_rgba(px, w, h).expect("valid icon")
}

/// Fire-and-forget POST to the local UI server (the same API the page uses).
fn api(base: &str, token: &str, path: &str) {
    let (base, token, path) = (base.to_string(), token.to_string(), path.to_string());
    std::thread::spawn(move || {
        let _ = ureq::post(&format!("{base}{path}")).set("X-Token", &token).send_string("{}");
    });
}

/// Tell the user something went wrong. This program has no console, so errors would otherwise vanish.
fn alert(message: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
        let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        // SAFETY: both buffers are NUL-terminated UTF-16 that outlive the call; a null owner window is allowed.
        unsafe { MessageBoxW(std::ptr::null_mut(), wide(message).as_ptr(), wide("AI Session Vault").as_ptr(), MB_OK | MB_ICONERROR) };
    }
    #[cfg(not(windows))]
    eprintln!("{message}");
}

/// What the command line asked for.
struct Args {
    url: Option<String>,
    hidden: bool,
    quit_on_close: bool,
    signal: Option<PathBuf>,
}

fn parse_args() -> Args {
    let mut a = Args { url: None, hidden: false, quit_on_close: false, signal: None };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--hidden" => a.hidden = true,
            "--quit-on-close" => a.quit_on_close = true,
            "--signal" => a.signal = it.next().map(PathBuf::from),
            _ if a.url.is_none() => a.url = Some(x),
            _ => {}
        }
    }
    a
}

/// No link: start the engine side (`avault app`) without a console window, then step aside.
fn launch_engine(hidden: bool) -> Result<()> {
    let exe = std::env::current_exe()?.with_file_name(if cfg!(windows) { "avault.exe" } else { "avault" });
    if !exe.exists() {
        bail!("avault was not found next to this program ({}). Reinstall AI Session Vault.", exe.display());
    }
    let mut c = std::process::Command::new(&exe);
    c.arg("app");
    if hidden {
        c.arg("--hidden");
    }
    c.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0800_0000); // CREATE_NO_WINDOW
    c.spawn().with_context(|| format!("cannot start {}", exe.display()))?;
    Ok(())
}

fn main() {
    let args = parse_args();
    let launcher = args.url.is_none();
    if let Err(e) = run(args) {
        // Started by hand it tells the user; started by `avault app` it reports through the exit code.
        if launcher {
            alert(&format!("{e:#}"));
        }
        std::process::exit(1);
    }
}

fn run(args: Args) -> Result<()> {
    let Some(url) = args.url.clone() else { return launch_engine(args.hidden) };
    // http://127.0.0.1:PORT/?t=TOKEN  (or .../setup.html?t=TOKEN)
    let (base, token) = match url.split_once("?t=") {
        Some((b, t)) => (b.rsplit_once('/').map_or(b, |(host, _)| host).to_string(), t.to_string()),
        None => bail!("not a vault link: {url}"),
    };

    let event_loop = EventLoopBuilder::<()>::new().build();
    let window = WindowBuilder::new()
        .with_title(if args.quit_on_close { "Set up AI Session Vault" } else { "AI Session Vault" })
        .with_visible(!args.hidden || args.quit_on_close)
        .with_window_icon({
            let (px, w, h) = logo();
            tao::window::Icon::from_rgba(px, w, h).ok()
        })
        .with_inner_size(LogicalSize::new(1360.0, 860.0))
        .with_min_inner_size(LogicalSize::new(900.0, 640.0))
        .build(&event_loop)
        .map_err(|e| anyhow!("cannot create the window: {e}"))?;

    let data = std::env::temp_dir().join("ai-vault-webview");
    let mut ctx = wry::WebContext::new(Some(data));
    let _view = wry::WebViewBuilder::with_web_context(&mut ctx)
        .with_url(&url)
        .build(&window)
        .map_err(|e| anyhow!("cannot start the webview (on Windows this needs the WebView2 runtime): {e}"))?;

    let menu = Menu::new();
    let open = MenuItem::new("Open AI Session Vault", true, None);
    let sync = MenuItem::new("Sync now", true, None);
    let pause = MenuItem::new("Pause syncing", true, None);
    let resume = MenuItem::new("Resume syncing", true, None);
    let quit = MenuItem::new("Quit", true, None);
    menu.append_items(&[&open, &PredefinedMenuItem::separator(), &sync, &pause, &resume, &PredefinedMenuItem::separator(), &quit]).map_err(|e| anyhow!("{e}"))?;
    // Setup has no engine to control, and closing its window should end it, so it gets no tray icon.
    let _tray = if args.quit_on_close {
        None
    } else {
        Some(
            TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip("AI Session Vault")
                .with_icon(icon())
                .build()
                .map_err(|e| anyhow!("cannot create the tray icon: {e}"))?,
        )
    };

    let (open_id, sync_id, pause_id, resume_id, quit_id) = (open.id().clone(), sync.id().clone(), pause.id().clone(), resume.id().clone(), quit.id().clone());
    let (menu_rx, tray_rx) = (MenuEvent::receiver(), TrayIconEvent::receiver());

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::WaitUntil(std::time::Instant::now() + std::time::Duration::from_millis(150));
        let show = || {
            window.set_visible(true);
            window.set_minimized(false);
            window.set_focus();
        };
        // A second launch of the app leaves this file to say "come to the front".
        if args.signal.as_ref().is_some_and(|p| std::fs::remove_file(p).is_ok()) {
            show();
        }
        while let Ok(e) = menu_rx.try_recv() {
            if e.id == open_id {
                show();
            } else if e.id == sync_id {
                api(&base, &token, "/api/sync");
            } else if e.id == pause_id {
                api(&base, &token, "/api/pause");
            } else if e.id == resume_id {
                api(&base, &token, "/api/resume");
            } else if e.id == quit_id {
                *flow = ControlFlow::Exit;
            }
        }
        while let Ok(e) = tray_rx.try_recv() {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                show();
            }
        }
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            if args.quit_on_close {
                *flow = ControlFlow::Exit;
            } else {
                window.set_visible(false); // keep syncing from the tray
            }
        }
    });
}
