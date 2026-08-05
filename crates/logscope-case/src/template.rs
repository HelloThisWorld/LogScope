//! Deterministic message-template extraction (v0.4 WP2, `template.mask`
//! algorithm v1).
//!
//! Design decision (ADR-0020): **pure rule-based masking, no adaptive
//! clustering.** Every record is normalized independently by versioned
//! deterministic rules; the template IS the masked token sequence, and a
//! pattern is the set of records sharing it. This is order-independent
//! by construction — the randomized-partition/parallelism determinism
//! gate holds trivially — and fully explainable: every active rule is
//! visible. The accepted limitation is the absence of fuzzy merging:
//! messages that differ beyond the mask rules form distinct patterns.
//!
//! Identity contract (all documented, all versioned together as
//! mask-set v1):
//! - input is the canonical UTF-8 text as ingested; no additional
//!   Unicode normalization is applied in v1 (combining-form variants
//!   produce distinct templates — documented limitation);
//! - case-sensitive; whitespace runs collapse to single separators and
//!   line endings count as whitespace (multiline messages: the template
//!   covers the whole text as one token stream);
//! - tokens are whitespace-split; leading `([{'"` and trailing
//!   `)]}.,;:!?'"` characters are detached as decoration, the core is
//!   classified, and the decoration is re-attached;
//! - rules apply per token in the FIXED order quoted → url → path →
//!   timestamp → uuid → trace/span hex → ip:port → ip → 0x-hex →
//!   duration → byte-size → decimal → integer → bare-hex; the first
//!   matching enabled rule wins;
//! - bounds: messages longer than [`MAX_MESSAGE_BYTES`] are cut at a
//!   char boundary and tokens beyond [`MAX_TOKENS`] are dropped; both
//!   truncations append an explicit `<truncated>` token so a truncated
//!   message can never collide with an untruncated equal prefix.

use serde::{Deserialize, Serialize};

use crate::CaseError;

/// Algorithm identity for message templates.
pub const TEMPLATE_ALGORITHM_ID: &str = "template.mask";
pub const TEMPLATE_ALGORITHM_VERSION: i64 = 1;
/// Mask-rule-set version (all rules below version together).
pub const MASK_SET_VERSION: i64 = 1;

pub const MAX_MESSAGE_BYTES: usize = 8 * 1024;
pub const MAX_TOKENS: usize = 512;

/// Built-in mask rules, each independently switchable. Defaults are all
/// enabled. This is analysis identity only — never export redaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct MaskSet {
    pub quoted: bool,
    pub url: bool,
    pub path: bool,
    pub timestamp: bool,
    pub uuid: bool,
    pub trace_span_hex: bool,
    pub ip: bool,
    pub hex0x: bool,
    pub duration: bool,
    pub byte_size: bool,
    pub number: bool,
    pub bare_hex: bool,
}

impl Default for MaskSet {
    fn default() -> Self {
        MaskSet {
            quoted: true,
            url: true,
            path: true,
            timestamp: true,
            uuid: true,
            trace_span_hex: true,
            ip: true,
            hex0x: true,
            duration: true,
            byte_size: true,
            number: true,
            bare_hex: true,
        }
    }
}

impl MaskSet {
    /// Parses a masking profile JSON (`{}` = defaults). Unknown keys are
    /// structured refusals, mirroring the redaction posture rule.
    pub fn parse(masking_profile_json: &str) -> Result<MaskSet, CaseError> {
        let trimmed = masking_profile_json.trim();
        if trimmed.is_empty() || trimmed == "{}" {
            return Ok(MaskSet::default());
        }
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|e| CaseError::Invalid(format!("masking profile does not parse: {e}")))?;
        if !value.is_object() {
            return Err(CaseError::Invalid(
                "masking profile must be a JSON object".into(),
            ));
        }
        serde_json::from_value(value)
            .map_err(|e| CaseError::Invalid(format!("masking profile does not parse: {e}")))
    }
}

/// One normalized message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateOutcome {
    /// The masked token sequence joined by single spaces.
    pub template: String,
    pub tokens: usize,
    /// True when the byte or token bound cut the input (an explicit
    /// `<truncated>` token is part of the template in that case).
    pub truncated: bool,
    /// True when at least one token was masked.
    pub changed: bool,
}

/// Normalizes one message under the mask set. Deterministic and total:
/// any string in, exactly one template out.
pub fn normalize_message(text: &str, masks: &MaskSet) -> TemplateOutcome {
    let (body, byte_truncated) = if text.len() > MAX_MESSAGE_BYTES {
        let mut end = MAX_MESSAGE_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        (&text[..end], true)
    } else {
        (text, false)
    };

    let mut out: Vec<String> = Vec::new();
    let mut changed = false;
    let mut token_truncated = false;
    for (i, raw) in body.split_whitespace().enumerate() {
        if i >= MAX_TOKENS {
            token_truncated = true;
            break;
        }
        // Whole-token quoting is checked BEFORE decoration stripping
        // (stripping removes the quotes). A quoted value spanning
        // several whitespace-split tokens is not recognized in v1
        // (documented limitation).
        if masks.quoted && is_quoted(raw) {
            changed = true;
            out.push("<q>".into());
            continue;
        }
        let (lead, core, trail) = split_decoration(raw);
        let masked = classify(core, masks);
        if let Some(mask) = masked {
            changed = true;
            out.push(format!("{lead}{mask}{trail}"));
        } else {
            out.push(raw.to_string());
        }
    }
    let truncated = byte_truncated || token_truncated;
    if truncated {
        out.push("<truncated>".into());
    }
    TemplateOutcome {
        tokens: out.len(),
        template: out.join(" "),
        truncated,
        changed,
    }
}

/// Splits leading/trailing decoration characters off a token.
fn split_decoration(token: &str) -> (&str, &str, &str) {
    let lead_end = token
        .char_indices()
        .find(|(_, c)| !matches!(c, '(' | '[' | '{' | '\'' | '"' | '<'))
        .map(|(i, _)| i)
        .unwrap_or(token.len());
    let (lead, rest) = token.split_at(lead_end);
    let trail_start = rest
        .char_indices()
        .rev()
        .take_while(|(_, c)| {
            matches!(
                c,
                ')' | ']' | '}' | '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '>'
            )
        })
        .last()
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    let (core, trail) = rest.split_at(trail_start);
    (lead, core, trail)
}

fn classify(core: &str, m: &MaskSet) -> Option<&'static str> {
    if core.is_empty() {
        return None;
    }
    if m.url && is_url(core) {
        return Some("<url>");
    }
    if m.path && is_path(core) {
        return Some("<path>");
    }
    if m.timestamp && is_timestamp(core) {
        return Some("<ts>");
    }
    if m.uuid && is_uuid(core) {
        return Some("<uuid>");
    }
    if m.trace_span_hex && is_trace_span_hex(core) {
        return Some("<id>");
    }
    if m.ip {
        if is_ip_port(core) {
            return Some("<ip>:<port>");
        }
        if is_ip(core) {
            return Some("<ip>");
        }
    }
    if m.hex0x && is_hex0x(core) {
        return Some("<hex>");
    }
    if m.duration && is_duration(core) {
        return Some("<dur>");
    }
    if m.byte_size && is_byte_size(core) {
        return Some("<size>");
    }
    if m.number {
        if is_decimal(core) {
            return Some("<num>");
        }
        if is_integer(core) {
            return Some("<num>");
        }
    }
    if m.bare_hex && is_bare_hex(core) {
        return Some("<hex>");
    }
    None
}

fn is_quoted(s: &str) -> bool {
    s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
}

fn is_url(s: &str) -> bool {
    let lower_scheme = s.split("://").next().unwrap_or("");
    s.contains("://")
        && !lower_scheme.is_empty()
        && lower_scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
}

fn is_path(s: &str) -> bool {
    // Unix-ish or Windows-ish: at least two separators, or a drive root.
    let unixish = s.starts_with('/') && s[1..].contains('/');
    let winish = (s.len() > 3
        && s.as_bytes()[1] == b':'
        && (s.as_bytes()[2] == b'\\' || s.as_bytes()[2] == b'/')
        && s.chars().next().unwrap().is_ascii_alphabetic())
        || s.matches('\\').count() >= 2;
    unixish || winish
}

fn is_timestamp(s: &str) -> bool {
    // ISO-ish date or date-time prefix: 4 digits, dash, 2, dash, 2.
    let b = s.as_bytes();
    b.len() >= 10
        && b[0..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
        || is_clock(s)
}

fn is_clock(s: &str) -> bool {
    // HH:MM:SS with optional fraction.
    let b = s.as_bytes();
    b.len() >= 8
        && b[0..2].iter().all(u8::is_ascii_digit)
        && b[2] == b':'
        && b[3..5].iter().all(u8::is_ascii_digit)
        && b[5] == b':'
        && b[6..8].iter().all(u8::is_ascii_digit)
        && (b.len() == 8 || b[8] == b'.' || b[8] == b',')
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn is_trace_span_hex(s: &str) -> bool {
    (s.len() == 16 || s.len() == 32)
        && s.bytes().all(|b| b.is_ascii_hexdigit())
        && s.bytes().any(|b| b.is_ascii_digit())
}

fn is_ip(s: &str) -> bool {
    // IPv4 dotted quad, or bracketless IPv6 (two-plus colons, hex groups).
    let v4 = {
        let parts: Vec<&str> = s.split('.').collect();
        parts.len() == 4
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit()))
    };
    let v6 = s.matches(':').count() >= 2
        && !s.is_empty()
        && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b':')
        && s.bytes().any(|b| b.is_ascii_hexdigit());
    v4 || v6
}

fn is_ip_port(s: &str) -> bool {
    match s.rsplit_once(':') {
        Some((host, port)) => {
            !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit()) && {
                let parts: Vec<&str> = host.split('.').collect();
                parts.len() == 4
                    && parts.iter().all(|p| {
                        !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit())
                    })
            }
        }
        None => false,
    }
}

fn is_hex0x(s: &str) -> bool {
    let rest = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"));
    matches!(rest, Some(r) if !r.is_empty() && r.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn is_duration(s: &str) -> bool {
    const UNITS: &[&str] = &["ns", "us", "µs", "ms", "s", "m", "h"];
    for unit in UNITS {
        if let Some(num) = s.strip_suffix(unit) {
            if !num.is_empty()
                && num.bytes().all(|b| b.is_ascii_digit() || b == b'.')
                && num.bytes().any(|b| b.is_ascii_digit())
            {
                return true;
            }
        }
    }
    false
}

fn is_byte_size(s: &str) -> bool {
    const UNITS: &[&str] = &["B", "KB", "KiB", "MB", "MiB", "GB", "GiB", "TB", "TiB"];
    for unit in UNITS {
        if let Some(num) = s.strip_suffix(unit) {
            if !num.is_empty()
                && num.bytes().all(|b| b.is_ascii_digit() || b == b'.')
                && num.bytes().any(|b| b.is_ascii_digit())
            {
                return true;
            }
        }
    }
    false
}

fn is_integer(s: &str) -> bool {
    let core = s.strip_prefix(['-', '+']).unwrap_or(s);
    !core.is_empty() && core.bytes().all(|b| b.is_ascii_digit())
}

fn is_decimal(s: &str) -> bool {
    let core = s.strip_prefix(['-', '+']).unwrap_or(s);
    match core.split_once('.') {
        Some((a, b)) => {
            !a.is_empty()
                && !b.is_empty()
                && a.bytes().all(|c| c.is_ascii_digit())
                && b.bytes().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

fn is_bare_hex(s: &str) -> bool {
    s.len() >= 8
        && s.bytes().all(|b| b.is_ascii_hexdigit())
        && s.bytes().any(|b| b.is_ascii_digit())
        && s.bytes().any(|b| b.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(text: &str) -> String {
        normalize_message(text, &MaskSet::default()).template
    }

    #[test]
    fn every_builtin_rule_masks_its_form() {
        assert_eq!(
            t("handler 42 finished in 12ms with 3.5MiB at 2026-08-05T10:00:00Z"),
            "handler <num> finished in <dur> with <size> at <ts>"
        );
        assert_eq!(
            t("request 550e8400-e29b-41d4-a716-446655440000 from 10.1.2.3:8080"),
            "request <uuid> from <ip>:<port>"
        );
        assert_eq!(
            t("trace 4bf92f3577b34da6a3ce929d0e0e4736 span 00f067aa0ba902b7"),
            "trace <id> span <id>"
        );
        assert_eq!(
            t("wrote /var/log/app/x.log and C:\\temp\\out.txt"),
            "wrote <path> and <path>"
        );
        assert_eq!(t("GET https://example.test/a?b=1 done"), "GET <url> done");
        assert_eq!(t("value \"secret\" ptr 0xDEADBEEF"), "value <q> ptr <hex>");
        assert_eq!(
            t("rate 3.14 count -7 addr deadbeef99"),
            "rate <num> count <num> addr <hex>"
        );
        assert_eq!(t("at 10:15:30.123 job ran"), "at <ts> job ran");
    }

    #[test]
    fn decoration_is_preserved_around_masked_cores() {
        assert_eq!(t("(id=abc took 12ms)."), "(id=abc took <dur>).");
        assert_eq!(t("[2026-08-05] boot"), "[<ts>] boot");
    }

    #[test]
    fn disabling_a_rule_keeps_the_literal_and_changes_identity() {
        let masks = MaskSet {
            number: false,
            ..MaskSet::default()
        };
        let out = normalize_message("handler 42 done", &masks);
        assert_eq!(out.template, "handler 42 done");
        assert!(!out.changed);
        let on = normalize_message("handler 42 done", &MaskSet::default());
        assert_ne!(out.template, on.template);
    }

    #[test]
    fn identical_messages_share_templates_and_whitespace_collapses() {
        assert_eq!(t("a  b\t c\nd"), "a b c d");
        assert_eq!(t("handler 1 ok"), t("handler 2 ok"));
        assert_ne!(t("handler 1 ok"), t("handler 1 failed"));
    }

    #[test]
    fn truncation_is_explicit_and_collision_free() {
        let long = "x".repeat(MAX_MESSAGE_BYTES + 100);
        let out = normalize_message(&long, &MaskSet::default());
        assert!(out.truncated);
        assert!(out.template.ends_with("<truncated>"));
        let many: String = (0..MAX_TOKENS + 10).map(|i| format!("w{i} ")).collect();
        let out = normalize_message(&many, &MaskSet::default());
        assert!(out.truncated);
        assert_eq!(out.tokens, MAX_TOKENS + 1, "cap plus the explicit marker");
        // An untruncated message equal to the truncated prefix stays distinct.
        let prefix = normalize_message(
            &(0..MAX_TOKENS)
                .map(|i| format!("w{i} "))
                .collect::<String>(),
            &MaskSet::default(),
        );
        assert_ne!(out.template, prefix.template);
    }

    #[test]
    fn masking_profile_parsing_is_strict() {
        assert_eq!(MaskSet::parse("{}").unwrap(), MaskSet::default());
        assert_eq!(MaskSet::parse("").unwrap(), MaskSet::default());
        let custom = MaskSet::parse("{\"number\":false}").unwrap();
        assert!(!custom.number);
        assert!(custom.uuid);
        assert!(MaskSet::parse("{\"surprise\":1}").is_err());
        assert!(MaskSet::parse("[]").is_err());
        assert!(MaskSet::parse("nope").is_err());
    }

    #[test]
    fn arbitrary_garbage_never_panics() {
        let mut state = 0x2026_0805_u64;
        let pool: Vec<char> = "ab1.:/\\\"'()[]{}<>-_ é🦀\u{0}\t\n".chars().collect();
        for _ in 0..2000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let len = (state % 60) as usize;
            let s: String = (0..len)
                .map(|i| pool[((state >> (i % 32)) as usize + i) % pool.len()])
                .collect();
            let a = normalize_message(&s, &MaskSet::default());
            let b = normalize_message(&s, &MaskSet::default());
            assert_eq!(a, b, "normalization is a pure function");
        }
    }
}
