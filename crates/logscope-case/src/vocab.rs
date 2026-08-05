//! Typed vocabulary for the investigation domain.
//!
//! Every enum has one canonical wire/storage string (snake_case). The
//! SQLite layer stores these strings; services parse and validate at the
//! boundary, so an unknown value is a structured error, never silent
//! coercion.

use serde::{Deserialize, Serialize};

macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        $name:ident, expected = $expected:literal {
            $($(#[$vmeta:meta])* $variant:ident => $s:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($(#[$vmeta])* $variant),+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];
            /// Human-readable list of accepted values for error messages.
            pub const EXPECTED: &'static str = $expected;

            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $s),+
                }
            }

            pub fn parse(s: &str) -> Option<Self> {
                match s {
                    $($s => Some($name::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

string_enum! {
    /// Investigation lifecycle. Transitions are explicit user actions;
    /// nothing is marked mitigated/resolved automatically.
    InvestigationStatus, expected = "open|investigating|mitigated|resolved|archived" {
        Open => "open",
        Investigating => "investigating",
        Mitigated => "mitigated",
        Resolved => "resolved",
        Archived => "archived",
    }
}

string_enum! {
    /// Generic product severity scale (documented in the user guide):
    /// sev1 = critical impact, sev2 = major, sev3 = minor, sev4 = low.
    /// Deliberately not an organization-specific scheme.
    Severity, expected = "sev1|sev2|sev3|sev4" {
        Sev1 => "sev1",
        Sev2 => "sev2",
        Sev3 => "sev3",
        Sev4 => "sev4",
    }
}

string_enum! {
    /// Hypothesis states are manual and audited. `Supported` is evidence
    /// leaning toward the hypothesis; `Confirmed` is a stronger explicit
    /// human judgment — the two are never merged.
    HypothesisState, expected = "unverified|supported|rejected|confirmed" {
        Unverified => "unverified",
        Supported => "supported",
        Rejected => "rejected",
        Confirmed => "confirmed",
    }
}

string_enum! {
    /// Typed investigation items. All v0.3 findings are user-authored.
    ItemKind, expected = "note|task|finding|question" {
        Note => "note",
        Task => "task",
        Finding => "finding",
        Question => "question",
    }
}

string_enum! {
    TaskStatus, expected = "todo|doing|done|dropped" {
        Todo => "todo",
        Doing => "doing",
        Done => "done",
        Dropped => "dropped",
    }
}

string_enum! {
    QuestionStatus, expected = "open|answered|deferred" {
        Open => "open",
        Answered => "answered",
        Deferred => "deferred",
    }
}

string_enum! {
    /// Manual timeline markers. Deployments, configuration changes, and
    /// operator actions are never inferred from log text.
    MarkerKind, expected = "deployment|config_change|operator_action|custom" {
        Deployment => "deployment",
        ConfigChange => "config_change",
        OperatorAction => "operator_action",
        Custom => "custom",
    }
}

string_enum! {
    /// Evidence kinds implemented in v0.3. Future kinds (comparison,
    /// deterministic finding, metric point/range, span, trace) extend the
    /// envelope version instead of changing these meanings.
    EvidenceKind, expected = "event|selection|query|explorer_group|histogram_interval|item_ref" {
        Event => "event",
        Selection => "selection",
        Query => "query",
        ExplorerGroup => "explorer_group",
        HistogramInterval => "histogram_interval",
        ItemRef => "item_ref",
    }
}

string_enum! {
    SignalKind, expected = "log|manual" {
        Log => "log",
        Manual => "manual",
    }
}

string_enum! {
    /// Evidence resolver integrity states. Distinct conditions stay
    /// distinct — the UI never collapses these into a green/red boolean.
    ResolverState, expected = "unverified|unsupported_reference_version|broken|dataset_revision_unavailable|source_missing|source_changed|canonical_available_source_unavailable|partially_resolved|query_drift|verified" {
        /// Verification has not completed (or was cancelled first).
        Unverified => "unverified",
        /// The reference cannot be interpreted safely by this build.
        UnsupportedReferenceVersion => "unsupported_reference_version",
        /// Neither the canonical reference nor a usable captured snapshot
        /// is available.
        Broken => "broken",
        /// The recorded dataset revision cannot be resolved (dataset
        /// deleted or its segment set is gone).
        DatasetRevisionUnavailable => "dataset_revision_unavailable",
        /// Canonical record resolves; the registered source file is gone.
        SourceMissing => "source_missing",
        /// Source file exists but no longer matches the captured identity.
        SourceChanged => "source_changed",
        /// Canonical data resolves; original bytes cannot be checked.
        CanonicalAvailableSourceUnavailable => "canonical_available_source_unavailable",
        /// Only part of a bounded selection resolves.
        PartiallyResolved => "partially_resolved",
        /// The query runs, but result metadata no longer matches capture.
        QueryDrift => "query_drift",
        /// Reference resolves and every expected fingerprint matches.
        Verified => "verified",
    }
}

string_enum! {
    /// Relevant-scope reference kinds.
    ScopeRefKind, expected = "dataset|resource_selector|saved_query|embedded_query|time_window|label" {
        Dataset => "dataset",
        ResourceSelector => "resource_selector",
        SavedQuery => "saved_query",
        EmbeddedQuery => "embedded_query",
        TimeWindow => "time_window",
        Label => "label",
    }
}

string_enum! {
    /// Deterministic analysis kinds (v0.4). Each kind has its own
    /// algorithm/rule versions; new kinds extend the definition schema
    /// version rather than reinterpreting these.
    AnalysisKind, expected = "message_pattern|stack_fingerprint|comparison|correlation|finding_rules" {
        MessagePattern => "message_pattern",
        StackFingerprint => "stack_fingerprint",
        Comparison => "comparison",
        Correlation => "correlation",
        FindingRules => "finding_rules",
    }
}

string_enum! {
    /// Analysis run lifecycle. `completed` is the only usable-result
    /// state; `stale` applies only to completed runs whose inputs later
    /// changed (history preserved). Cancellation and failure are never
    /// representable as an empty success.
    AnalysisRunState, expected = "pending|running|completed|cancelled|failed|stale" {
        Pending => "pending",
        Running => "running",
        Completed => "completed",
        Cancelled => "cancelled",
        Failed => "failed",
        Stale => "stale",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects_unknown() {
        for s in InvestigationStatus::ALL {
            assert_eq!(InvestigationStatus::parse(s.as_str()), Some(*s));
        }
        for s in ResolverState::ALL {
            assert_eq!(ResolverState::parse(s.as_str()), Some(*s));
        }
        assert_eq!(InvestigationStatus::parse("closed"), None);
        assert_eq!(HypothesisState::parse("proved"), None);
        assert_eq!(ResolverState::parse("ok"), None);
    }

    #[test]
    fn serde_uses_the_same_snake_case_strings() {
        let json = serde_json::to_string(&MarkerKind::ConfigChange).unwrap();
        assert_eq!(json, "\"config_change\"");
        let back: MarkerKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, MarkerKind::ConfigChange);
        assert_eq!(
            serde_json::to_string(&ResolverState::CanonicalAvailableSourceUnavailable).unwrap(),
            "\"canonical_available_source_unavailable\""
        );
    }
}
