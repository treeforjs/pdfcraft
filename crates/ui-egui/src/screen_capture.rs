//! Create ▸ Screen capture: the operating system's own capture tool takes the picture (the
//! whole screen, a window the user clicks, or a region the user drags), and the image becomes a
//! new one-page PDF (`Session::pdf_from_capture`).
//!
//! PdfCraft links no capture library: it runs the tool the system already has, on a worker
//! thread so an interactive capture never blocks a frame. macOS ships `screencapture`; on Linux
//! and the BSDs the first of GNOME Screenshot, Spectacle, grim (whole screen only), scrot or
//! ImageMagick's `import` that is installed is used. Windows has no command-line capture tool:
//! there Windows PowerShell (part of Windows) copies the whole screen itself, and for a window
//! or region it opens the system's Snipping bar (`ms-screenclip:`) and takes the snip from the
//! clipboard once the clipboard changes. For a whole-screen capture PdfCraft minimizes its own
//! window first so it isn't in the picture, and restores it afterwards.

use crate::PdfCraftApp;

/// What to capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMode {
    /// The whole screen.
    Screen,
    /// A window the user picks.
    Window,
    /// A rectangle the user drags.
    Region,
}

/// A capture tool and its arguments; the output path goes last.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureTool {
    pub program: &'static str,
    pub args: &'static [&'static str],
    /// The output path goes in the `PDFCRAFT_CAPTURE_OUT` environment variable instead of last
    /// among the arguments (the PowerShell scripts).
    pub out_env: bool,
}

/// Windows PowerShell: copy the whole (virtual) screen to a PNG.
const WINDOWS_SCREEN: &str = "$ErrorActionPreference='Stop';Add-Type -AssemblyName System.Windows.Forms,System.Drawing;\
$b=[System.Windows.Forms.SystemInformation]::VirtualScreen;$i=New-Object System.Drawing.Bitmap $b.Width,$b.Height;\
$g=[System.Drawing.Graphics]::FromImage($i);$g.CopyFromScreen($b.Left,$b.Top,0,0,$i.Size);\
$i.Save($env:PDFCRAFT_CAPTURE_OUT,[System.Drawing.Imaging.ImageFormat]::Png)";

/// Windows PowerShell: open the Snipping bar and save the snip it puts on the clipboard (the
/// clipboard's sequence number tells a new snip from what was there before). Exit status 1
/// when nothing arrives within two minutes (the user cancelled).
const WINDOWS_SNIP: &str = "$ErrorActionPreference='Stop';Add-Type -AssemblyName System.Windows.Forms,System.Drawing;\
Add-Type -Namespace PdfCraft -Name Clip -MemberDefinition '[DllImport(\"user32.dll\")] public static extern uint GetClipboardSequenceNumber();';\
$n=[PdfCraft.Clip]::GetClipboardSequenceNumber();Start-Process 'ms-screenclip:';$t=[DateTime]::Now.AddMinutes(2);\
while([DateTime]::Now -lt $t){Start-Sleep -Milliseconds 250;\
if([PdfCraft.Clip]::GetClipboardSequenceNumber() -ne $n -and [System.Windows.Forms.Clipboard]::ContainsImage()){\
[System.Windows.Forms.Clipboard]::GetImage().Save($env:PDFCRAFT_CAPTURE_OUT,[System.Drawing.Imaging.ImageFormat]::Png);exit 0}};exit 1";

/// The capture tool for `os` (`std::env::consts::OS`) and `mode`, the first that `installed`
/// says exists; `None` when there is none.
pub fn capture_tool(os: &str, mode: CaptureMode, installed: &dyn Fn(&str) -> bool) -> Option<CaptureTool> {
    use CaptureMode::{Region, Screen, Window};
    let tool = |program, args| CaptureTool { program, args, out_env: false };
    let candidates: Vec<CaptureTool> = match os {
        // -x: no sound; -i: interactive; -w: windows only; -s: a dragged rectangle only.
        "macos" => vec![tool(
            "/usr/sbin/screencapture",
            match mode {
                Screen => &["-x"],
                Window => &["-x", "-i", "-w"],
                Region => &["-x", "-i", "-s"],
            },
        )],
        "linux" | "freebsd" | "openbsd" | "netbsd" | "dragonfly" => {
            let mut v = vec![
                // GNOME Screenshot captures the focused window, so give the user time to pick one.
                tool(
                    "gnome-screenshot",
                    match mode {
                        Screen => &["-f"],
                        Window => &["-w", "-d", "3", "-f"],
                        Region => &["-a", "-f"],
                    },
                ),
                // Spectacle: background mode, no notification; the window under the cursor.
                tool(
                    "spectacle",
                    match mode {
                        Screen => &["-b", "-n", "-f", "-o"],
                        Window => &["-b", "-n", "-d", "3000", "-u", "-o"],
                        Region => &["-b", "-n", "-r", "-o"],
                    },
                ),
            ];
            if mode == Screen {
                v.push(tool("grim", &[]));
            }
            v.push(tool("scrot", if mode == Screen { &[] } else { &["-s"] }));
            // `import` without -window lets the user click a window or drag a rectangle.
            v.push(tool("import", if mode == Screen { &["-window", "root"] } else { &[] }));
            v
        }
        // -STA: the clipboard needs a single-threaded apartment.
        "windows" => vec![CaptureTool {
            program: "powershell.exe",
            args: match mode {
                Screen => &["-NoProfile", "-NonInteractive", "-STA", "-ExecutionPolicy", "Bypass", "-Command", WINDOWS_SCREEN],
                Window | Region => &["-NoProfile", "-NonInteractive", "-STA", "-ExecutionPolicy", "Bypass", "-Command", WINDOWS_SNIP],
            },
            out_env: true,
        }],
        _ => Vec::new(),
    };
    candidates.into_iter().find(|t| installed(t.program))
}

/// Whether `program` (an absolute path, or a name looked up on `PATH`) exists.
#[cfg(not(target_arch = "wasm32"))]
fn installed(program: &str) -> bool {
    let path = std::path::Path::new(program);
    if path.is_absolute() {
        return path.is_file();
    }
    std::env::var_os("PATH").is_some_and(|dirs| std::env::split_paths(&dirs).any(|d| d.join(program).is_file()))
}

/// Run `tool`, writing to a fresh temporary file, and return the image (`None`: the user
/// cancelled, so nothing was written).
#[cfg(not(target_arch = "wasm32"))]
fn run_capture(tool: &CaptureTool) -> Result<Option<Vec<u8>>, String> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let out = std::env::temp_dir().join(format!("pdfcraft-capture-{}-{nanos}.png", std::process::id()));
    let mut command = std::process::Command::new(tool.program);
    command.args(tool.args);
    if tool.out_env {
        command.env("PDFCRAFT_CAPTURE_OUT", &out);
    } else {
        command.arg(&out);
    }
    let status = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("{} could not be started: {e}", tool.program))?;
    let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    let read = if size == 0 {
        // No picture: the user pressed Escape (most tools then exit with an error status).
        Ok(None)
    } else if size > pdfcraft_engine::clip::MAX_CAPTURE_BYTES as u64 {
        Err(format!("the capture is too large ({size} bytes)"))
    } else {
        std::fs::read(&out).map(Some).map_err(|e| format!("the capture could not be read: {e}"))
    };
    let _ = std::fs::remove_file(&out);
    match read {
        Ok(None) if !status.success() && status.code() != Some(1) => Err(format!("{} failed ({status})", tool.program)),
        other => other,
    }
}

/// Where a capture is.
#[derive(Default)]
pub(crate) enum Capture {
    #[default]
    Idle,
    #[cfg(not(target_arch = "wasm32"))]
    Running(std::sync::mpsc::Receiver<Result<Option<Vec<u8>>, String>>),
}

impl PdfCraftApp {
    /// Create ▸ Screen / Window / Region capture: start the system's capture tool.
    pub(crate) fn capture_screen(&mut self, mode: CaptureMode) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if matches!(self.capture, Capture::Running(_)) {
                return;
            }
            let Some(tool) = capture_tool(std::env::consts::OS, mode, &installed) else {
                self.notify_tr("No screen capture tool was found. Capture with your system's tool, then use Create PDF from clipboard.");
                return;
            };
            let (tx, rx) = std::sync::mpsc::channel();
            let ctx = self.ctx.clone();
            // The whole screen without PdfCraft's own window over it: minimize it first and give
            // the system a moment to animate it away.
            let hide = mode == CaptureMode::Screen && !self.run_inline;
            if hide && let Some(ctx) = &ctx {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            self.capture_hid_window = hide && ctx.is_some();
            let spawned = std::thread::Builder::new().name("pdfcraft-capture".into()).spawn(move || {
                if hide {
                    std::thread::sleep(std::time::Duration::from_millis(600));
                }
                let result = std::panic::catch_unwind(|| run_capture(&tool)).unwrap_or_else(|_| Err("the capture tool failed unexpectedly".into()));
                // The receiver may be gone (the app quit): nothing to report to then.
                let _ = tx.send(result);
                if let Some(ctx) = ctx {
                    ctx.request_repaint();
                }
            });
            if let Err(e) = spawned {
                self.restore_after_capture();
                self.notify_fmt("Couldn't capture the screen: {e}", &[("e", &e.to_string())]);
                return;
            }
            self.capture = Capture::Running(rx);
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = mode;
            self.notify_tr("No screen capture tool was found. Capture with your system's tool, then use Create PDF from clipboard.");
        }
    }

    /// Pick up a finished capture (each frame) and open it as a new document.
    pub(crate) fn poll_capture(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Capture::Running(rx) = &self.capture {
            let result = match rx.try_recv() {
                Ok(r) => r,
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("the capture stopped unexpectedly".into()),
            };
            self.capture = Capture::Idle;
            self.restore_after_capture();
            match result {
                Ok(Some(image)) => {
                    if let Err(e) = self.create_from_capture(&image, None) {
                        self.notify_fmt("Couldn't create a PDF: {e}", &[("e", &e)]);
                    }
                }
                Ok(None) => self.notify_tr("Screen capture cancelled"),
                Err(e) => self.notify_fmt("Couldn't capture the screen: {e}", &[("e", &e)]),
            }
        }
    }

    /// Bring the window back after a whole-screen capture minimized it.
    #[cfg(not(target_arch = "wasm32"))]
    fn restore_after_capture(&mut self) {
        if std::mem::take(&mut self.capture_hid_window)
            && let Some(ctx) = &self.ctx
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    /// A new document from a captured image (PNG, JPEG or TIFF), optionally cut to `region`.
    pub fn create_from_capture(&mut self, image: &[u8], region: Option<pdfcraft_engine::CaptureRegion>) -> Result<(), String> {
        let created = self.session.pdf_from_capture(image, region).map_err(|e| e.to_string());
        self.open_created_bytes("Screen capture.pdf", created)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_system_gets_its_own_capture_tool() {
        let all = |_: &str| true;
        let mac = capture_tool("macos", CaptureMode::Region, &all).unwrap();
        assert_eq!((mac.program, mac.args), ("/usr/sbin/screencapture", &["-x", "-i", "-s"][..]));
        assert_eq!(capture_tool("linux", CaptureMode::Window, &all).unwrap().program, "gnome-screenshot");
        let only = |name: &'static str| move |p: &str| p == name;
        assert_eq!(capture_tool("linux", CaptureMode::Screen, &only("grim")).unwrap().program, "grim");
        assert_eq!(capture_tool("linux", CaptureMode::Region, &only("grim")), None, "grim can't pick a region by itself");
        assert_eq!(capture_tool("freebsd", CaptureMode::Region, &only("import")).unwrap().args, &[] as &[&str]);
        let win = capture_tool("windows", CaptureMode::Screen, &all).unwrap();
        assert!(win.out_env && win.program == "powershell.exe" && win.args.contains(&"-STA"), "{win:?}");
        assert!(win.args.last().unwrap().contains("CopyFromScreen"));
        let snip = capture_tool("windows", CaptureMode::Region, &all).unwrap();
        assert!(snip.args.last().unwrap().contains("ms-screenclip:") && snip.args.last().unwrap().contains("GetClipboardSequenceNumber"));
        assert_eq!(capture_tool("windows", CaptureMode::Window, &all), Some(snip));
        assert_eq!(capture_tool("windows", CaptureMode::Screen, &|_| false), None);
        assert!(!mac.out_env);
        assert_eq!(capture_tool("haiku", CaptureMode::Screen, &all), None);
        assert_eq!(capture_tool("linux", CaptureMode::Screen, &|_| false), None);
    }
}
