//! S443 / ADR-0092 — the probe-ingestion source abstraction.
//!
//! The real DMG MORI / Renishaw transports are STUBBED in this session
//! (no specific machine wired yet — mirrors the S441 DÁP scaffolding
//! pattern). The manual-entry inspection pipeline (verdict + auto-NCR)
//! works TODAY without any probe; when a real source lands it feeds the
//! SAME `record_inspection` pipeline via these events.
//!
//! Research gap (ADR-0092 §Open #1 / research §2.1): base MTConnect
//! carries a measured VALUE (`SAMPLE`, `subType="ACTUAL"`), not a
//! verdict — ABERP computes the tier. The real impls below must poll
//! `/sample?from=<nextSequence>` gap-safe and map probe `Sensor` items.

use thiserror::Error;
use time::OffsetDateTime;

/// An opaque polling cursor. For MTConnect this is the `nextSequence`
/// from the streams `<Header>`; a source advances it on each poll so the
/// next call reads contiguously (gap-safe catch-up).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProbeCursor(pub u64);

/// One raw measurement straight off a probe source, BEFORE ABERP
/// computes a verdict. The pass/fail tier is NOT carried here — it is
/// derived in code against the inspection plan ([[trust-code-not-operator]]).
#[derive(Debug, Clone, PartialEq)]
pub struct RawProbeEvent {
    /// Source-system-unique id (dedupe of replays). MTConnect: the
    /// `sequence`; Renishaw Central: the result record id.
    pub source_event_id: String,
    pub timestamp_utc: OffsetDateTime,
    pub probe_serial: String,
    /// Operator-set on the probe cycle; matched to a plan `feature_name`.
    pub feature_name: String,
    pub actual_value: f64,
    pub units: String,
    pub cycle_id: Option<String>,
    pub machine_identifier: String,
    pub last_calibration_at_utc: OffsetDateTime,
}

#[derive(Debug, Error)]
pub enum ProbeError {
    #[error("probe transport: {0}")]
    Transport(String),
    #[error("probe parse: {0}")]
    Parse(String),
}

/// A source of probe measurements. One `poll` returns the events newer
/// than `since` plus the cursor to pass next time.
pub trait ProbeIngestionSource {
    fn poll_probe_events(
        &self,
        since: ProbeCursor,
    ) -> Result<(Vec<RawProbeEvent>, ProbeCursor), ProbeError>;
}

/// Test + dev-mode demo source. Returns its canned events on the first
/// poll and advances the cursor past them; a poll at-or-past the high
/// watermark returns nothing.
#[derive(Debug, Clone)]
pub struct MockProbeSource {
    events: Vec<RawProbeEvent>,
    /// The cursor value the events are considered to sit "above".
    base: u64,
}

impl MockProbeSource {
    pub fn new(events: Vec<RawProbeEvent>) -> Self {
        Self { events, base: 0 }
    }
}

impl ProbeIngestionSource for MockProbeSource {
    fn poll_probe_events(
        &self,
        since: ProbeCursor,
    ) -> Result<(Vec<RawProbeEvent>, ProbeCursor), ProbeError> {
        let high = self.base + self.events.len() as u64;
        if since.0 >= high {
            return Ok((Vec::new(), since));
        }
        Ok((self.events.clone(), ProbeCursor(high)))
    }
}

/// MTConnect probe ingestion — the v1 PRIMARY transport (ADR-0092). The
/// struct is constructible (so it compiles into the binary and can be
/// wired once a real machine is available), but ingestion is a stub: the
/// first implementation step is a `/probe` + `/current` capture from the
/// target DMG MORI machine to confirm probe `Sensor` items are on the
/// wire (research gap #1).
#[derive(Debug, Clone)]
pub struct MtconnectProbeSource {
    /// Base agent URL, e.g. `http://cnc-line-a-1:5000`.
    pub agent_url: String,
    /// The probe `Sensor` component name to subscribe to.
    pub probe_component: String,
}

impl MtconnectProbeSource {
    pub fn new(agent_url: impl Into<String>, probe_component: impl Into<String>) -> Self {
        Self {
            agent_url: agent_url.into(),
            probe_component: probe_component.into(),
        }
    }
}

impl ProbeIngestionSource for MtconnectProbeSource {
    fn poll_probe_events(
        &self,
        _since: ProbeCursor,
    ) -> Result<(Vec<RawProbeEvent>, ProbeCursor), ProbeError> {
        todo!(
            "real MTConnect probe ingestion — pending specific machine + probe access; \
             ADR-0092 §Decision documents the spec gaps (poll /sample?from=<nextSequence> \
             gap-safe, map probe Sensor SAMPLE subType=ACTUAL items to RawProbeEvent)"
        )
    }
}

/// Renishaw Central API ingestion — a future-tier source for shops whose
/// metrology data flows through Renishaw Central rather than the machine's
/// MTConnect agent (ADR-0092 §2.2). Constructible, stubbed.
#[derive(Debug, Clone)]
pub struct RenishawCentralSource {
    /// Renishaw Central API base URL.
    pub api_base_url: String,
}

impl RenishawCentralSource {
    pub fn new(api_base_url: impl Into<String>) -> Self {
        Self {
            api_base_url: api_base_url.into(),
        }
    }
}

impl ProbeIngestionSource for RenishawCentralSource {
    fn poll_probe_events(
        &self,
        _since: ProbeCursor,
    ) -> Result<(Vec<RawProbeEvent>, ProbeCursor), ProbeError> {
        todo!(
            "real Renishaw Central API ingestion — pending Renishaw Central account; \
             ADR-0092 §Decision documents the integration shape"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::format_description::well_known::Rfc3339;

    fn ev(id: &str, feature: &str, actual: f64) -> RawProbeEvent {
        RawProbeEvent {
            source_event_id: id.into(),
            timestamp_utc: OffsetDateTime::parse("2026-06-17T09:14:22Z", &Rfc3339).unwrap(),
            probe_serial: "RMP600-007".into(),
            feature_name: feature.into(),
            actual_value: actual,
            units: "mm".into(),
            cycle_id: Some("CYCLE977".into()),
            machine_identifier: "cnc-line-a-1".into(),
            last_calibration_at_utc: OffsetDateTime::parse("2026-06-17T06:00:00Z", &Rfc3339)
                .unwrap(),
        }
    }

    #[test]
    fn mock_source_returns_canned_events_then_drains() {
        let src = MockProbeSource::new(vec![
            ev("200417", "Bore Ø", 25.038),
            ev("200421", "Face Z", 0.012),
        ]);
        let (events, cursor) = src.poll_probe_events(ProbeCursor::default()).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(cursor, ProbeCursor(2));
        // A second poll at the high watermark yields nothing.
        let (events2, cursor2) = src.poll_probe_events(cursor).unwrap();
        assert!(events2.is_empty());
        assert_eq!(cursor2, cursor);
    }

    #[test]
    #[should_panic(expected = "real MTConnect probe ingestion")]
    fn mtconnect_source_is_constructible_but_poll_is_a_stub() {
        let src = MtconnectProbeSource::new("http://cnc-line-a-1:5000", "touch-probe");
        // Constructing it is fine (compiles into the binary); polling panics.
        let _ = src.poll_probe_events(ProbeCursor::default());
    }

    #[test]
    #[should_panic(expected = "real Renishaw Central API ingestion")]
    fn renishaw_source_is_constructible_but_poll_is_a_stub() {
        let src = RenishawCentralSource::new("https://central.example/api");
        let _ = src.poll_probe_events(ProbeCursor::default());
    }
}
