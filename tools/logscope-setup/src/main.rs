//! LogScope offline graphical extractor (ADR-0018).
//!
//! A launcher stub with the canonical payload appended, in the spirit of a
//! self-contained Java archive. It is deliberately **not** an installer: no
//! MSI/MSIX, no registered uninstaller, no service, scheduled task, startup
//! entry, firewall rule, `PATH` change, file association or mandatory
//! registry state. It asks for a destination, extracts, verifies every file
//! against `package-manifest.json`, and publishes atomically.
//!
//! No network access is performed at any point.

// GUI subsystem: without this, every launch drags a console window behind
// the dialogs. Windows-only so the shared-core CI legs still build the
// crate as an ordinary console binary.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod ui;

use logscope_setup::{extract, payload};

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use extract::{ExtractError, Progress};

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            ui::error(&message);
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate this program: {e}"))?;

    // Read and verify the payload before asking the user anything: a
    // corrupt download should fail immediately, not after they have picked
    // a folder and watched a progress bar.
    let payload = payload::read_appended(&exe).map_err(|e| e.to_string())?;

    let manifest_hint = format!(
        "LogScope will be extracted from this Setup file.\n\n\
         Payload SHA-256:\n{}\n\n\
         This copies files only. No installer, service, registry entry or \
         network connection is used.",
        payload.sha256
    );
    if !ui::confirm("LogScope Setup", &manifest_hint) {
        return Ok(());
    }

    let dest: PathBuf = match ui::pick_destination() {
        Some(d) => d,
        None => return Ok(()), // user cancelled the folder picker
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let mut window = ui::ProgressWindow::open("Extracting LogScope", cancel.clone());

    let cancel_flag = cancel.clone();
    let result = extract::extract_verified(
        &payload.bytes,
        &dest,
        |p| match p {
            Progress::Started { total } => {
                window.log(&format!("Extracting {total} files to {}", dest.display()));
                window.set_total(total);
            }
            Progress::File { index, total, path } => {
                window.set_position(index, total);
                window.log(path);
            }
            Progress::Verifying => window.log("Verifying every file against package-manifest.json"),
            Progress::Publishing => window.log("Publishing"),
            Progress::Done { root } => window.log(&format!("Done: {}", root.display())),
        },
        &move || cancel_flag.load(Ordering::SeqCst),
    );

    match result {
        Ok(manifest) => {
            window.close();
            ui::info(
                "LogScope Setup",
                &format!(
                    "{} {} was extracted and verified.\n\n{}\n\n\
                     To remove LogScope later, delete that folder. Your \
                     workspaces and sources are stored elsewhere and are not \
                     affected.",
                    manifest.name,
                    manifest.version,
                    dest.display()
                ),
            );
            Ok(())
        }
        Err(ExtractError::Cancelled) => {
            window.close();
            ui::info(
                "LogScope Setup",
                "Extraction was cancelled. Nothing was installed.",
            );
            Ok(())
        }
        Err(e) => {
            window.close();
            Err(format!(
                "{e}\n\nNothing was installed; the destination was left unchanged."
            ))
        }
    }
}
