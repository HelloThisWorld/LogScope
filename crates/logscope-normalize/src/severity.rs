//! Severity text mapping.
//!
//! Maps common severity tokens from Java (Logback/Log4j2/JUL), .NET, Python,
//! Go, Node.js, and syslog ecosystems to OTLP severity numbers. Unknown text
//! maps to `None` — the caller records a `SeverityUnmapped` flag and keeps
//! the original text. The mapping table is generic and public; it must never
//! contain organization-specific level names.

use logscope_model::severity::levels as severity;

/// Maps a severity token to an OTLP severity number.
///
/// Matching is case-insensitive on the trimmed token. Numeric strings that
/// already are valid OTLP severity numbers (1..=24) pass through.
pub fn map_severity_text(text: &str) -> Option<i32> {
    let token = text.trim();
    if token.is_empty() {
        return None;
    }
    if let Ok(n) = token.parse::<i32>() {
        if (1..=24).contains(&n) {
            return Some(n);
        }
        return None;
    }
    let upper = token.to_ascii_uppercase();
    Some(match upper.as_str() {
        // trace family
        "TRACE" | "VERBOSE" | "FINEST" => severity::TRACE,
        "FINER" => severity::TRACE2,
        // debug family
        "DEBUG" | "FINE" | "DBG" => severity::DEBUG,
        "CONFIG" => severity::DEBUG3,
        // info family
        "INFO" | "INFORMATION" | "INFORMATIONAL" => severity::INFO,
        "NOTICE" => severity::INFO2,
        // warn family
        "WARN" | "WARNING" => severity::WARN,
        // error family
        "ERROR" | "ERR" | "SEVERE" => severity::ERROR,
        // fatal family
        "FATAL" | "CRITICAL" | "CRIT" => severity::FATAL,
        "ALERT" => severity::FATAL2,
        "EMERGENCY" | "EMERG" | "PANIC" => severity::FATAL3,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_tokens_map() {
        assert_eq!(map_severity_text("INFO"), Some(9));
        assert_eq!(map_severity_text("info"), Some(9));
        assert_eq!(map_severity_text(" Warn "), Some(13));
        assert_eq!(map_severity_text("WARNING"), Some(13));
        assert_eq!(map_severity_text("SEVERE"), Some(17));
        assert_eq!(map_severity_text("CRITICAL"), Some(21));
        assert_eq!(map_severity_text("FINEST"), Some(1));
    }

    #[test]
    fn numeric_passthrough_only_in_otlp_range() {
        assert_eq!(map_severity_text("17"), Some(17));
        assert_eq!(map_severity_text("0"), None);
        assert_eq!(map_severity_text("50"), None);
    }

    #[test]
    fn unknown_is_none() {
        assert_eq!(map_severity_text("LOUD"), None);
        assert_eq!(map_severity_text(""), None);
    }
}
