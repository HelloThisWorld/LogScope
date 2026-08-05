//! Deterministic stack-trace fingerprinting (v0.4 WP2, `stack.frames`
//! algorithm v1) — for stack traces that arrive INSIDE log records.
//! This is log-body analysis, never runtime dump diagnostics.
//!
//! Contract (ADR-0020):
//! - Supported textual forms: Java, .NET, Python, Go panics, Node.js.
//!   Detection runs every parser and picks, in the FIXED order
//!   java → dotnet → python → go → node, the first fully-parsed result
//!   (else the first partial one) — deterministic by construction.
//! - Frame identity: the callable, qualified as the source provides it.
//!   Java/.NET keep the fully-qualified method and drop the file/line;
//!   Python/Node/Go keep `callable@file-basename`. Line numbers, column
//!   numbers, and addresses never participate, so equivalent traces
//!   with volatile locations share one fingerprint. Distinct exception
//!   types never merge, whatever the frames look like.
//! - Nested causes contribute their exception types in order (bounded by
//!   [`MAX_CAUSES`]); frames are bounded by [`MAX_FRAMES`] per trace.
//!   Exceeding either bound sets `truncated`, and truncation
//!   participates in the fingerprint identity so a partial trace never
//!   collides with its complete form.
//! - Malformed or partial traces fingerprint what parsed, with the
//!   parse quality reported honestly.

use serde::{Deserialize, Serialize};

/// Algorithm identity for stack fingerprints.
pub const STACK_ALGORITHM_ID: &str = "stack.frames";
pub const STACK_ALGORITHM_VERSION: i64 = 1;

pub const MAX_FRAMES: usize = 128;
pub const MAX_CAUSES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StackQuality {
    /// Exception type and at least one frame parsed.
    Parsed,
    /// Something recognizable parsed, but not both halves.
    Partial,
    /// No supported stack structure was recognized.
    Malformed,
}

/// One normalized stack, ready for identity hashing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackOutcome {
    /// Exception/error/panic type ("" when none parsed).
    pub exception_type: String,
    /// Ordered normalized frames, outermost first as printed.
    pub frames: Vec<String>,
    /// Nested cause exception types, in chain order.
    pub causes: Vec<String>,
    pub truncated: bool,
    pub quality: StackQuality,
    /// Which textual form matched ("java"|"dotnet"|"python"|"go"|"node"|"none").
    pub form: &'static str,
}

/// Parses one stack text deterministically. Total: any input yields an
/// outcome; garbage yields `Malformed` with empty identity parts.
pub fn parse_stack(text: &str) -> StackOutcome {
    let lines: Vec<&str> = text.lines().collect();
    let candidates = [
        parse_java(&lines),
        parse_dotnet(&lines),
        parse_python(&lines),
        parse_go(&lines),
        parse_node(&lines),
    ];
    for c in &candidates {
        if c.quality == StackQuality::Parsed {
            return c.clone();
        }
    }
    for c in &candidates {
        if c.quality == StackQuality::Partial {
            return c.clone();
        }
    }
    StackOutcome {
        exception_type: String::new(),
        frames: vec![],
        causes: vec![],
        truncated: false,
        quality: StackQuality::Malformed,
        form: "none",
    }
}

fn bounded(frames: &mut Vec<String>, truncated: &mut bool, frame: String) {
    if frames.len() < MAX_FRAMES {
        frames.push(frame);
    } else {
        *truncated = true;
    }
}

fn quality_of(exception_type: &str, frames: &[String]) -> StackQuality {
    match (!exception_type.is_empty(), !frames.is_empty()) {
        (true, true) => StackQuality::Parsed,
        (false, false) => StackQuality::Malformed,
        _ => StackQuality::Partial,
    }
}

fn file_basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// A plausible exception-type head: `pkg.Type` / `Type` ending in a
/// letter, first line, before an optional `: message`.
fn exception_head(line: &str) -> Option<String> {
    let head = line.split(':').next()?.trim();
    if head.is_empty() || head.contains(' ') {
        return None;
    }
    let ok = head
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '$');
    if ok && head.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        Some(head.to_string())
    } else {
        None
    }
}

// ---- Java --------------------------------------------------------------------

fn parse_java(lines: &[&str]) -> StackOutcome {
    let mut exception_type = String::new();
    let mut frames = Vec::new();
    let mut causes = Vec::new();
    let mut truncated = false;
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("at ") {
            // com.example.Class.method(File.java:123) — keep the method,
            // drop the location. Only java-shaped locations qualify, so
            // .NET ("(args) in file:line N") and Node ("(path.js:1:2)")
            // frames are never claimed by this parser.
            if let Some((callable, tail)) = rest.split_once('(') {
                let location = tail.trim_end_matches(')');
                let javaish = location == "Native Method"
                    || location == "Unknown Source"
                    || [".java:", ".kt:", ".scala:"].iter().any(|ext| {
                        location.rsplit_once(':').is_some_and(|(file, line_no)| {
                            file.ends_with(&ext[..ext.len() - 1])
                                && !line_no.is_empty()
                                && line_no.bytes().all(|b| b.is_ascii_digit())
                        })
                    });
                let callable = callable.trim();
                if javaish && callable.contains('.') {
                    bounded(&mut frames, &mut truncated, callable.to_string());
                    continue;
                }
            }
        }
        if let Some(rest) = line.strip_prefix("Caused by: ") {
            if let Some(t) = exception_head(rest) {
                if causes.len() < MAX_CAUSES {
                    causes.push(t);
                } else {
                    truncated = true;
                }
            }
            continue;
        }
        if line.starts_with("Suppressed: ") || line.starts_with("... ") {
            continue;
        }
        if i == 0 || exception_type.is_empty() {
            if let Some(t) = exception_head(line) {
                // Java types conventionally carry a package dot.
                if t.contains('.') && exception_type.is_empty() {
                    exception_type = t;
                }
            }
        }
    }
    // Java frames are dotted; a frame-less "type only" line is Partial.
    StackOutcome {
        quality: quality_of(&exception_type, &frames),
        exception_type,
        frames,
        causes,
        truncated,
        form: "java",
    }
}

// ---- .NET --------------------------------------------------------------------

fn parse_dotnet(lines: &[&str]) -> StackOutcome {
    let mut exception_type = String::new();
    let mut frames = Vec::new();
    let mut causes = Vec::new();
    let mut truncated = false;
    for raw in lines {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("at ") {
            // at Ns.Class.Method(args) in C:\f.cs:line 12
            let callable = rest.split('(').next().unwrap_or(rest).trim();
            if callable.contains('.') && !rest.contains(".java") && rest.contains(')') {
                let is_dotnet_shaped = rest.contains(" in ") || rest.ends_with(')');
                if is_dotnet_shaped {
                    bounded(&mut frames, &mut truncated, callable.to_string());
                    continue;
                }
            }
        }
        if let Some(rest) = line.strip_prefix("---> ") {
            if let Some(t) = exception_head(rest) {
                if causes.len() < MAX_CAUSES {
                    causes.push(t);
                } else {
                    truncated = true;
                }
            }
            continue;
        }
        if exception_type.is_empty() {
            if let Some(t) = exception_head(line) {
                if t.starts_with("System.") || t.contains("Exception") {
                    exception_type = t;
                }
            }
        }
    }
    StackOutcome {
        quality: quality_of(&exception_type, &frames),
        exception_type,
        frames,
        causes,
        truncated,
        form: "dotnet",
    }
}

// ---- Python ------------------------------------------------------------------

fn parse_python(lines: &[&str]) -> StackOutcome {
    let mut saw_traceback = false;
    let mut frames = Vec::new();
    let mut causes = Vec::new();
    let mut exception_type = String::new();
    let mut truncated = false;
    for raw in lines {
        let line = raw.trim();
        if line.starts_with("Traceback (most recent call last)") {
            if !exception_type.is_empty() {
                // A second traceback block: the earlier type is a cause.
                if causes.len() < MAX_CAUSES {
                    causes.push(std::mem::take(&mut exception_type));
                } else {
                    truncated = true;
                }
            }
            saw_traceback = true;
            continue;
        }
        if !saw_traceback {
            continue;
        }
        if let Some(rest) = line.strip_prefix("File \"") {
            // File "path", line 12, in fn
            if let Some((path, tail)) = rest.split_once('"') {
                let fun = tail.rsplit(" in ").next().unwrap_or("").trim();
                if !fun.is_empty() && !fun.contains(' ') {
                    bounded(
                        &mut frames,
                        &mut truncated,
                        format!("{fun}@{}", file_basename(path)),
                    );
                }
            }
            continue;
        }
        // The final line names the exception: "ValueError: msg" or bare.
        if let Some(t) = exception_head(line) {
            if !line.starts_with("File") && raw.starts_with(|c: char| !c.is_whitespace()) {
                exception_type = t;
            }
        }
    }
    StackOutcome {
        quality: if saw_traceback {
            quality_of(&exception_type, &frames)
        } else {
            StackQuality::Malformed
        },
        exception_type,
        frames,
        causes,
        truncated,
        form: "python",
    }
}

// ---- Go ----------------------------------------------------------------------

fn parse_go(lines: &[&str]) -> StackOutcome {
    let mut exception_type = String::new();
    let mut frames = Vec::new();
    let mut truncated = false;
    let mut in_goroutine = false;
    let mut pending_callable: Option<String> = None;
    for raw in lines {
        let line = raw.trim_end();
        if let Some(rest) = line.strip_prefix("panic: ") {
            if exception_type.is_empty() {
                let head = rest.split([':', '(']).next().unwrap_or(rest).trim();
                if !head.is_empty() {
                    exception_type = format!("panic:{}", head.replace(' ', "-"));
                }
            }
            continue;
        }
        if line.starts_with("goroutine ") && line.ends_with(':') {
            in_goroutine = true;
            continue;
        }
        if !in_goroutine {
            continue;
        }
        if !raw.starts_with(['\t', ' ']) {
            // pkg/path.fn(0x...) — callable line.
            let callable = line.split('(').next().unwrap_or(line).trim();
            if callable.contains('.') && !callable.is_empty() {
                pending_callable = Some(callable.to_string());
            }
            continue;
        }
        // "\tfile.go:123 +0x45" — location line completes the frame.
        if let Some(callable) = pending_callable.take() {
            let loc = line.trim();
            let file = loc.split(':').next().unwrap_or("");
            bounded(
                &mut frames,
                &mut truncated,
                format!("{callable}@{}", file_basename(file)),
            );
        }
    }
    StackOutcome {
        quality: quality_of(&exception_type, &frames),
        exception_type,
        frames,
        causes: vec![],
        truncated,
        form: "go",
    }
}

// ---- Node --------------------------------------------------------------------

fn parse_node(lines: &[&str]) -> StackOutcome {
    let mut exception_type = String::new();
    let mut frames = Vec::new();
    let mut truncated = false;
    for raw in lines {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("at ") {
            // at fn (path:1:2)  |  at path:1:2  |  at async fn (…)
            let rest = rest.strip_prefix("async ").unwrap_or(rest);
            let frame = match rest.split_once(" (") {
                Some((fun, loc)) => {
                    let path = loc.trim_end_matches(')');
                    let file = path.split(':').next().unwrap_or(path);
                    format!("{}@{}", fun.trim(), file_basename(file))
                }
                None => {
                    let file = rest.split(':').next().unwrap_or(rest);
                    format!("<anonymous>@{}", file_basename(file))
                }
            };
            bounded(&mut frames, &mut truncated, frame);
            continue;
        }
        if exception_type.is_empty() {
            if let Some(t) = exception_head(line) {
                if t.ends_with("Error") || t.ends_with("Exception") {
                    exception_type = t;
                }
            }
        }
    }
    StackOutcome {
        quality: quality_of(&exception_type, &frames),
        exception_type,
        frames,
        causes: vec![],
        truncated,
        form: "node",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JAVA: &str = "java.lang.IllegalStateException: broken pipeline\n\
        \tat com.example.Service.handle(Service.java:42)\n\
        \tat com.example.Loop.run(Loop.java:99)\n\
        Caused by: java.io.IOException: disk gone\n\
        \tat com.example.Disk.write(Disk.java:7)\n\
        \t... 12 more";

    #[test]
    fn java_traces_normalize_and_volatile_lines_do_not_matter() {
        let a = parse_stack(JAVA);
        assert_eq!(a.form, "java");
        assert_eq!(a.quality, StackQuality::Parsed);
        assert_eq!(a.exception_type, "java.lang.IllegalStateException");
        assert_eq!(a.causes, vec!["java.io.IOException"]);
        assert_eq!(a.frames[0], "com.example.Service.handle");
        // Same trace with different line numbers → identical identity.
        let b = parse_stack(&JAVA.replace(":42", ":137").replace(":99", ":1"));
        assert_eq!(a, b);
        // A different exception type never merges.
        let c = parse_stack(&JAVA.replace("IllegalStateException", "IllegalArgumentException"));
        assert_ne!(a.exception_type, c.exception_type);
    }

    #[test]
    fn python_traces_put_the_type_last_and_chain_causes() {
        let text = "Traceback (most recent call last):\n\
            \x20 File \"/srv/app/worker.py\", line 10, in run\n\
            \x20   handle()\n\
            \x20 File \"/srv/app/db.py\", line 55, in handle\n\
            \x20   raise ValueError(\"bad\")\n\
            ValueError: bad row";
        let out = parse_stack(text);
        assert_eq!(out.form, "python");
        assert_eq!(out.quality, StackQuality::Parsed);
        assert_eq!(out.exception_type, "ValueError");
        assert_eq!(out.frames, vec!["run@worker.py", "handle@db.py"]);

        let chained = format!(
            "{text}\n\nDuring handling of the above exception, another exception occurred:\n\n\
             Traceback (most recent call last):\n\
             \x20 File \"/srv/app/main.py\", line 3, in main\n\
             \x20   run()\n\
             RuntimeError: gave up"
        );
        let out = parse_stack(&chained);
        assert_eq!(out.exception_type, "RuntimeError");
        assert_eq!(out.causes, vec!["ValueError"]);
    }

    #[test]
    fn node_dotnet_and_go_forms_parse() {
        let node = "TypeError: Cannot read properties of undefined\n\
            \x20   at handle (/srv/app/routes/order.js:12:5)\n\
            \x20   at /srv/app/index.js:3:1";
        let out = parse_stack(node);
        assert_eq!(out.form, "node");
        assert_eq!(out.frames, vec!["handle@order.js", "<anonymous>@index.js"]);

        let dotnet = "System.InvalidOperationException: no handler\n\
            \x20  at Api.Orders.Submit(Order order) in C:\\src\\Orders.cs:line 88\n\
            \x20  at Api.Program.Main()";
        let out = parse_stack(dotnet);
        assert_eq!(out.form, "dotnet");
        assert_eq!(out.exception_type, "System.InvalidOperationException");
        assert_eq!(out.frames[0], "Api.Orders.Submit");

        let go = "panic: runtime error: index out of range\n\n\
            goroutine 7 [running]:\n\
            main.process(0xc000010200)\n\
            \t/srv/app/main.go:42 +0x1a\n\
            main.main()\n\
            \t/srv/app/main.go:12 +0x2b";
        let out = parse_stack(go);
        assert_eq!(out.form, "go");
        assert!(out.exception_type.starts_with("panic:runtime-error"));
        assert_eq!(
            out.frames,
            vec!["main.process@main.go", "main.main@main.go"]
        );
    }

    #[test]
    fn malformed_and_partial_states_are_honest_and_bounded() {
        let out = parse_stack("just an ordinary log line\nnothing stacky here");
        assert_eq!(out.quality, StackQuality::Malformed);
        assert_eq!(out.form, "none");

        // Type without frames is partial, not invented.
        let out = parse_stack("java.lang.OutOfMemoryError: heap");
        assert_eq!(out.quality, StackQuality::Partial);

        // Frame bound sets truncation, which is part of identity.
        let mut big = String::from("java.lang.Deep: x\n");
        for i in 0..(MAX_FRAMES + 10) {
            big.push_str(&format!("\tat com.example.F{i}.run(F.java:{i})\n"));
        }
        let out = parse_stack(&big);
        assert!(out.truncated);
        assert_eq!(out.frames.len(), MAX_FRAMES);
    }

    #[test]
    fn parsing_is_a_pure_function_over_garbage() {
        let mut state = 0x2026_0805_u64;
        let pool: Vec<char> = "at File\"():\\/\n\t .x1é".chars().collect();
        for _ in 0..1500 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let len = (state % 120) as usize;
            let s: String = (0..len)
                .map(|i| pool[((state >> (i % 24)) as usize + i) % pool.len()])
                .collect();
            assert_eq!(parse_stack(&s), parse_stack(&s));
        }
    }
}
