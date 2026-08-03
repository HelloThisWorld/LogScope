//! Build-time utility: append a payload to the extractor stub.
//!
//! Not shipped. It exists so the trailer layout has exactly one
//! implementation — the one the stub itself reads — instead of being
//! duplicated in a packaging script where it could silently drift.
//!
//! Usage:
//!   append-payload <stub.exe> <payload.zip> <out.exe>
//!
//! On success it prints the payload SHA-256, which the caller can compare
//! against the standalone archive's checksum to prove the two public
//! artifacts carry byte-identical payloads.

use std::path::PathBuf;
use std::process::ExitCode;

use logscope_setup::payload::{build_trailer, read_appended};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        eprintln!("usage: append-payload <stub.exe> <payload.zip> <out.exe>");
        return ExitCode::FAILURE;
    }
    let (stub, payload, out) = (
        PathBuf::from(&args[0]),
        PathBuf::from(&args[1]),
        PathBuf::from(&args[2]),
    );

    match run(&stub, &payload, &out) {
        Ok(sha) => {
            println!("{sha}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("append-payload: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(stub: &PathBuf, payload: &PathBuf, out: &PathBuf) -> Result<String, String> {
    let stub_bytes = std::fs::read(stub).map_err(|e| format!("{}: {e}", stub.display()))?;
    let payload_bytes =
        std::fs::read(payload).map_err(|e| format!("{}: {e}", payload.display()))?;

    let mut combined = Vec::with_capacity(stub_bytes.len() + payload_bytes.len() + 70);
    combined.extend_from_slice(&stub_bytes);
    combined.extend_from_slice(&payload_bytes);
    combined.extend_from_slice(&build_trailer(&payload_bytes, stub_bytes.len() as u64));

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(out, &combined).map_err(|e| format!("{}: {e}", out.display()))?;

    // Read it back through the same code path the stub uses. Writing a
    // setup executable that the stub cannot open is precisely the failure
    // this utility exists to prevent, so it is verified here rather than
    // discovered by a user.
    let verified = read_appended(out).map_err(|e| format!("verification failed: {e}"))?;
    if verified.bytes != payload_bytes {
        return Err("payload read back from the setup executable differs from the input".into());
    }
    Ok(verified.sha256)
}
