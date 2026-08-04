//! Minimal Win32 front end: folder picker, message dialogs and a progress
//! window with a live extraction log.
//!
//! Kept deliberately small and separable from `extract`/`payload`, which
//! hold the behavior that matters and are unit-tested. This module is
//! presentation only: it must not decide anything about safety, staging or
//! verification.
//!
//! On non-Windows targets the whole module degrades to stderr logging so
//! the crate still builds and its core stays testable in shared-core CI.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::atomic::Ordering;

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Controls::{
        InitCommonControlsEx, ICC_PROGRESS_CLASS, INITCOMMONCONTROLSEX, PBM_SETPOS, PBM_SETRANGE32,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn info(title: &str, message: &str) {
        rfd::MessageDialog::new()
            .set_title(title)
            .set_description(message)
            .set_level(rfd::MessageLevel::Info)
            .show();
    }

    pub fn error(message: &str) {
        rfd::MessageDialog::new()
            .set_title("LogScope Setup")
            .set_description(message)
            .set_level(rfd::MessageLevel::Error)
            .show();
    }

    pub fn confirm(title: &str, message: &str) -> bool {
        matches!(
            rfd::MessageDialog::new()
                .set_title(title)
                .set_description(message)
                .set_buttons(rfd::MessageButtons::OkCancel)
                .show(),
            rfd::MessageDialogResult::Ok
        )
    }

    pub fn pick_destination() -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("Choose where to extract LogScope")
            .pick_folder()
    }

    const ID_PROGRESS: isize = 1001;
    const ID_LOG: isize = 1002;
    const ID_CANCEL: isize = 1003;

    pub struct ProgressWindow {
        hwnd: HWND,
        progress: HWND,
        log: HWND,
        cancel: Arc<AtomicBool>,
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_COMMAND if (wparam.0 & 0xffff) as isize == ID_CANCEL => {
                // The flag lives in the window's userdata so the pump and
                // the worker thread agree without a global.
                let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const AtomicBool;
                if !ptr.is_null() {
                    unsafe { &*ptr }.store(true, Ordering::SeqCst);
                }
                LRESULT(0)
            }
            // Closing the window is a cancel request, never a silent kill.
            WM_CLOSE => {
                let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const AtomicBool;
                if !ptr.is_null() {
                    unsafe { &*ptr }.store(true, Ordering::SeqCst);
                }
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    impl ProgressWindow {
        pub fn open(title: &str, cancel: Arc<AtomicBool>) -> Self {
            unsafe {
                // The manifest binds comctl32 v6; v6 additionally requires
                // the class to be registered explicitly before
                // `msctls_progress32` can be created.
                let icc = INITCOMMONCONTROLSEX {
                    dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                    dwICC: ICC_PROGRESS_CLASS,
                };
                let _ = InitCommonControlsEx(&icc);

                let instance = GetModuleHandleW(None).unwrap_or_default();
                let class = w!("LogScopeSetupProgress");
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(wndproc),
                    hInstance: instance.into(),
                    lpszClassName: class,
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    ..Default::default()
                };
                RegisterClassW(&wc);

                let hwnd = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    class,
                    PCWSTR(wide(title).as_ptr()),
                    WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_VISIBLE,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    620,
                    360,
                    None,
                    None,
                    Some(instance.into()),
                    None,
                )
                .unwrap_or_default();

                // Cancel flag is reachable from the window procedure.
                let raw = Arc::into_raw(cancel.clone());
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize);

                let progress = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("msctls_progress32"),
                    PCWSTR::null(),
                    WS_CHILD | WS_VISIBLE,
                    12,
                    12,
                    580,
                    22,
                    Some(hwnd),
                    Some(HMENU(ID_PROGRESS as *mut _)),
                    Some(instance.into()),
                    None,
                )
                .unwrap_or_default();

                let log = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("LISTBOX"),
                    PCWSTR::null(),
                    WS_CHILD | WS_VISIBLE | WS_VSCROLL | WINDOW_STYLE(LBS_NOSEL as u32),
                    12,
                    46,
                    580,
                    230,
                    Some(hwnd),
                    Some(HMENU(ID_LOG as *mut _)),
                    Some(instance.into()),
                    None,
                )
                .unwrap_or_default();

                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("BUTTON"),
                    w!("Cancel"),
                    WS_CHILD | WS_VISIBLE,
                    496,
                    286,
                    96,
                    28,
                    Some(hwnd),
                    Some(HMENU(ID_CANCEL as *mut _)),
                    Some(instance.into()),
                    None,
                );

                ProgressWindow {
                    hwnd,
                    progress,
                    log,
                    cancel,
                }
            }
        }

        /// Drains pending messages so the window repaints and the Cancel
        /// button stays responsive while extraction runs on this thread.
        fn pump(&self) {
            unsafe {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }

        pub fn set_total(&mut self, total: usize) {
            unsafe {
                SendMessageW(
                    self.progress,
                    PBM_SETRANGE32,
                    Some(WPARAM(0)),
                    Some(LPARAM(total.max(1) as isize)),
                );
            }
            self.pump();
        }

        pub fn set_position(&mut self, index: usize, _total: usize) {
            unsafe {
                SendMessageW(
                    self.progress,
                    PBM_SETPOS,
                    Some(WPARAM(index)),
                    Some(LPARAM(0)),
                );
            }
            self.pump();
        }

        pub fn log(&mut self, line: &str) {
            unsafe {
                let text = wide(line);
                let idx = SendMessageW(
                    self.log,
                    LB_ADDSTRING,
                    Some(WPARAM(0)),
                    Some(LPARAM(text.as_ptr() as isize)),
                );
                // Keep the newest line visible.
                SendMessageW(
                    self.log,
                    LB_SETTOPINDEX,
                    Some(WPARAM(idx.0.max(0) as usize)),
                    Some(LPARAM(0)),
                );
            }
            self.pump();
        }

        pub fn close(&mut self) {
            unsafe {
                let raw = SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0) as *const AtomicBool;
                if !raw.is_null() {
                    drop(Arc::from_raw(raw));
                }
                let _ = DestroyWindow(self.hwnd);
            }
            self.pump();
            let _ = &self.cancel;
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn info(title: &str, message: &str) {
        eprintln!("[{title}] {message}");
    }
    pub fn error(message: &str) {
        eprintln!("[error] {message}");
    }
    /// Non-interactive fallback: never proceeds implicitly.
    pub fn confirm(title: &str, message: &str) -> bool {
        eprintln!("[{title}] {message}");
        false
    }
    pub fn pick_destination() -> Option<PathBuf> {
        None
    }

    pub struct ProgressWindow {
        _cancel: Arc<AtomicBool>,
    }

    impl ProgressWindow {
        pub fn open(title: &str, cancel: Arc<AtomicBool>) -> Self {
            eprintln!("[{title}]");
            ProgressWindow { _cancel: cancel }
        }
        pub fn set_total(&mut self, total: usize) {
            eprintln!("total: {total}");
        }
        pub fn set_position(&mut self, index: usize, total: usize) {
            eprintln!("{index}/{total}");
        }
        pub fn log(&mut self, line: &str) {
            eprintln!("{line}");
        }
        pub fn close(&mut self) {}
    }
}

pub use imp::{confirm, error, info, pick_destination, ProgressWindow};
