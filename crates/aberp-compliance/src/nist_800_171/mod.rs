//! NIST SP 800-171 (Rev. 2) control identifiers (DFARS 252.204-7012).
//!
//! All 110 security requirements across the 14 families (3.1 Access Control
//! … 3.14 System and Information Integrity), as `&'static str` constants of
//! the form `"<dotted-id>: <short title>"`. These are *tags*: a future audit
//! `EventKind` that contributes evidence toward a specific control references
//! the corresponding constant, so a System Security Plan / assessment can
//! trace ledger events back to the control they satisfy.
//!
//! The constant name encodes the family abbreviation + the dotted id, e.g.
//! [`AC_3_1_1`]. The titles are the official short requirement statements,
//! condensed for use as labels (the full text lives in NIST SP 800-171 Rev. 2
//! §3). [`ALL_CONTROLS`] enumerates every constant for iteration / counting.
//!
//! Family abbreviations: AC (Access Control), AT (Awareness & Training),
//! AU (Audit & Accountability), CM (Configuration Management), IA
//! (Identification & Authentication), IR (Incident Response), MA
//! (Maintenance), MP (Media Protection), PS (Personnel Security), PE
//! (Physical Protection), RA (Risk Assessment), CA (Security Assessment),
//! SC (System & Communications Protection), SI (System & Information
//! Integrity).

// ── 3.1 Access Control (AC) — 22 controls ───────────────────────────────
pub const AC_3_1_1: &str = "3.1.1: Limit system access to authorized users and devices";
pub const AC_3_1_2: &str = "3.1.2: Limit system access to permitted transactions and functions";
pub const AC_3_1_3: &str = "3.1.3: Control the flow of CUI per approved authorizations";
pub const AC_3_1_4: &str = "3.1.4: Separate the duties of individuals to reduce collusion risk";
pub const AC_3_1_5: &str = "3.1.5: Employ least privilege, including for privileged accounts";
pub const AC_3_1_6: &str = "3.1.6: Use non-privileged accounts for nonsecurity functions";
pub const AC_3_1_7: &str =
    "3.1.7: Prevent non-privileged users from executing privileged functions";
pub const AC_3_1_8: &str = "3.1.8: Limit unsuccessful logon attempts";
pub const AC_3_1_9: &str = "3.1.9: Provide privacy and security notices per CUI rules";
pub const AC_3_1_10: &str =
    "3.1.10: Use session lock with pattern-hiding displays after inactivity";
pub const AC_3_1_11: &str = "3.1.11: Terminate a user session after a defined condition";
pub const AC_3_1_12: &str = "3.1.12: Monitor and control remote access sessions";
pub const AC_3_1_13: &str = "3.1.13: Use cryptography to protect remote access sessions";
pub const AC_3_1_14: &str = "3.1.14: Route remote access via managed access control points";
pub const AC_3_1_15: &str = "3.1.15: Authorize remote execution of privileged commands";
pub const AC_3_1_16: &str = "3.1.16: Authorize wireless access prior to connection";
pub const AC_3_1_17: &str = "3.1.17: Protect wireless access using authentication and encryption";
pub const AC_3_1_18: &str = "3.1.18: Control connection of mobile devices";
pub const AC_3_1_19: &str = "3.1.19: Encrypt CUI on mobile devices and platforms";
pub const AC_3_1_20: &str = "3.1.20: Verify and limit connections to external systems";
pub const AC_3_1_21: &str = "3.1.21: Limit use of portable storage devices on external systems";
pub const AC_3_1_22: &str =
    "3.1.22: Control CUI posted or processed on publicly accessible systems";

// ── 3.2 Awareness and Training (AT) — 3 controls ────────────────────────
pub const AT_3_2_1: &str = "3.2.1: Make personnel aware of security risks and applicable policies";
pub const AT_3_2_2: &str = "3.2.2: Train personnel to carry out their security-related duties";
pub const AT_3_2_3: &str = "3.2.3: Provide insider-threat awareness training";

// ── 3.3 Audit and Accountability (AU) — 9 controls ──────────────────────
pub const AU_3_3_1: &str = "3.3.1: Create and retain system audit logs and records";
pub const AU_3_3_2: &str = "3.3.2: Ensure user actions can be uniquely traced to the user";
pub const AU_3_3_3: &str = "3.3.3: Review and update logged events";
pub const AU_3_3_4: &str = "3.3.4: Alert on audit logging process failure";
pub const AU_3_3_5: &str = "3.3.5: Correlate audit record review, analysis, and reporting";
pub const AU_3_3_6: &str = "3.3.6: Provide audit record reduction and report generation";
pub const AU_3_3_7: &str = "3.3.7: Synchronize internal clocks with an authoritative time source";
pub const AU_3_3_8: &str = "3.3.8: Protect audit information and tools from unauthorized access";
pub const AU_3_3_9: &str = "3.3.9: Limit management of audit logging to privileged users";

// ── 3.4 Configuration Management (CM) — 9 controls ──────────────────────
pub const CM_3_4_1: &str = "3.4.1: Establish and maintain baseline configurations and inventories";
pub const CM_3_4_2: &str = "3.4.2: Establish and enforce security configuration settings";
pub const CM_3_4_3: &str = "3.4.3: Track, review, approve, and log changes to systems";
pub const CM_3_4_4: &str = "3.4.4: Analyze the security impact of changes before implementation";
pub const CM_3_4_5: &str = "3.4.5: Define and enforce access restrictions for changes";
pub const CM_3_4_6: &str = "3.4.6: Employ least functionality — only essential capabilities";
pub const CM_3_4_7: &str = "3.4.7: Restrict nonessential programs, ports, protocols, and services";
pub const CM_3_4_8: &str = "3.4.8: Apply deny-by-exception / permit-by-exception software policy";
pub const CM_3_4_9: &str = "3.4.9: Control and monitor user-installed software";

// ── 3.5 Identification and Authentication (IA) — 11 controls ────────────
pub const IA_3_5_1: &str = "3.5.1: Identify system users, processes, and devices";
pub const IA_3_5_2: &str = "3.5.2: Authenticate identities before allowing access";
pub const IA_3_5_3: &str =
    "3.5.3: Use multifactor authentication for privileged and network access";
pub const IA_3_5_4: &str = "3.5.4: Employ replay-resistant authentication mechanisms";
pub const IA_3_5_5: &str = "3.5.5: Prevent reuse of identifiers for a defined period";
pub const IA_3_5_6: &str = "3.5.6: Disable identifiers after a period of inactivity";
pub const IA_3_5_7: &str = "3.5.7: Enforce minimum password complexity on creation";
pub const IA_3_5_8: &str = "3.5.8: Prohibit password reuse for a number of generations";
pub const IA_3_5_9: &str = "3.5.9: Allow temporary passwords with immediate permanent change";
pub const IA_3_5_10: &str = "3.5.10: Store and transmit only cryptographically-protected passwords";
pub const IA_3_5_11: &str = "3.5.11: Obscure feedback of authentication information";

// ── 3.6 Incident Response (IR) — 3 controls ─────────────────────────────
pub const IR_3_6_1: &str = "3.6.1: Establish an operational incident-handling capability";
pub const IR_3_6_2: &str = "3.6.2: Track, document, and report incidents to designated officials";
pub const IR_3_6_3: &str = "3.6.3: Test the organizational incident response capability";

// ── 3.7 Maintenance (MA) — 6 controls ───────────────────────────────────
pub const MA_3_7_1: &str = "3.7.1: Perform maintenance on organizational systems";
pub const MA_3_7_2: &str = "3.7.2: Control the tools, techniques, and personnel for maintenance";
pub const MA_3_7_3: &str = "3.7.3: Sanitize equipment removed for off-site maintenance of CUI";
pub const MA_3_7_4: &str = "3.7.4: Check diagnostic media for malicious code before use";
pub const MA_3_7_5: &str = "3.7.5: Require MFA for nonlocal maintenance sessions";
pub const MA_3_7_6: &str = "3.7.6: Supervise maintenance personnel without access authorization";

// ── 3.8 Media Protection (MP) — 9 controls ──────────────────────────────
pub const MP_3_8_1: &str = "3.8.1: Protect system media (paper and digital) containing CUI";
pub const MP_3_8_2: &str = "3.8.2: Limit access to CUI on system media to authorized users";
pub const MP_3_8_3: &str = "3.8.3: Sanitize or destroy media containing CUI before disposal";
pub const MP_3_8_4: &str = "3.8.4: Mark media with CUI markings and distribution limitations";
pub const MP_3_8_5: &str = "3.8.5: Control access to and account for media during transport";
pub const MP_3_8_6: &str = "3.8.6: Encrypt CUI on digital media during transport";
pub const MP_3_8_7: &str = "3.8.7: Control the use of removable media on system components";
pub const MP_3_8_8: &str = "3.8.8: Prohibit portable storage devices with no identifiable owner";
pub const MP_3_8_9: &str = "3.8.9: Protect the confidentiality of backup CUI at storage locations";

// ── 3.9 Personnel Security (PS) — 2 controls ────────────────────────────
pub const PS_3_9_1: &str = "3.9.1: Screen individuals before authorizing access to CUI";
pub const PS_3_9_2: &str = "3.9.2: Protect CUI during and after personnel actions";

// ── 3.10 Physical Protection (PE) — 6 controls ──────────────────────────
pub const PE_3_10_1: &str = "3.10.1: Limit physical access to systems and operating environments";
pub const PE_3_10_2: &str = "3.10.2: Protect and monitor the physical facility and infrastructure";
pub const PE_3_10_3: &str = "3.10.3: Escort visitors and monitor visitor activity";
pub const PE_3_10_4: &str = "3.10.4: Maintain audit logs of physical access";
pub const PE_3_10_5: &str = "3.10.5: Control and manage physical access devices";
pub const PE_3_10_6: &str = "3.10.6: Enforce safeguarding of CUI at alternate work sites";

// ── 3.11 Risk Assessment (RA) — 3 controls ──────────────────────────────
pub const RA_3_11_1: &str =
    "3.11.1: Periodically assess risk to operations, assets, and individuals";
pub const RA_3_11_2: &str = "3.11.2: Scan for vulnerabilities in systems and applications";
pub const RA_3_11_3: &str = "3.11.3: Remediate vulnerabilities per risk assessments";

// ── 3.12 Security Assessment (CA) — 4 controls ──────────────────────────
pub const CA_3_12_1: &str = "3.12.1: Periodically assess the security controls for effectiveness";
pub const CA_3_12_2: &str = "3.12.2: Develop and implement plans of action to correct deficiencies";
pub const CA_3_12_3: &str = "3.12.3: Monitor security controls on an ongoing basis";
pub const CA_3_12_4: &str = "3.12.4: Develop and update system security plans";

// ── 3.13 System and Communications Protection (SC) — 16 controls ────────
pub const SC_3_13_1: &str = "3.13.1: Monitor, control, and protect communications at boundaries";
pub const SC_3_13_2: &str = "3.13.2: Employ secure architectural and engineering design principles";
pub const SC_3_13_3: &str =
    "3.13.3: Separate user functionality from system management functionality";
pub const SC_3_13_4: &str =
    "3.13.4: Prevent unauthorized information transfer via shared resources";
pub const SC_3_13_5: &str = "3.13.5: Implement subnetworks for publicly accessible components";
pub const SC_3_13_6: &str = "3.13.6: Deny network traffic by default, permit by exception";
pub const SC_3_13_7: &str = "3.13.7: Prevent split tunneling on remote devices";
pub const SC_3_13_8: &str = "3.13.8: Encrypt CUI during transmission unless otherwise safeguarded";
pub const SC_3_13_9: &str =
    "3.13.9: Terminate network connections at session end or after inactivity";
pub const SC_3_13_10: &str = "3.13.10: Establish and manage cryptographic keys";
pub const SC_3_13_11: &str = "3.13.11: Employ FIPS-validated cryptography to protect CUI";
pub const SC_3_13_12: &str =
    "3.13.12: Prohibit remote activation of collaborative computing devices";
pub const SC_3_13_13: &str = "3.13.13: Control and monitor the use of mobile code";
pub const SC_3_13_14: &str = "3.13.14: Control and monitor the use of VoIP technologies";
pub const SC_3_13_15: &str = "3.13.15: Protect the authenticity of communications sessions";
pub const SC_3_13_16: &str = "3.13.16: Protect the confidentiality of CUI at rest";

// ── 3.14 System and Information Integrity (SI) — 7 controls ─────────────
pub const SI_3_14_1: &str = "3.14.1: Identify, report, and correct system flaws in a timely manner";
pub const SI_3_14_2: &str = "3.14.2: Provide malicious-code protection at designated locations";
pub const SI_3_14_3: &str = "3.14.3: Monitor security alerts and advisories and take action";
pub const SI_3_14_4: &str =
    "3.14.4: Update malicious-code protection when new releases are available";
pub const SI_3_14_5: &str =
    "3.14.5: Perform periodic and real-time scans of files from external sources";
pub const SI_3_14_6: &str = "3.14.6: Monitor systems and communications traffic to detect attacks";
pub const SI_3_14_7: &str = "3.14.7: Identify unauthorized use of organizational systems";

/// Every one of the 110 NIST SP 800-171 Rev. 2 control constants, in family
/// order. Provided for iteration, counting, and coverage checks.
pub const ALL_CONTROLS: [&str; 110] = [
    AC_3_1_1, AC_3_1_2, AC_3_1_3, AC_3_1_4, AC_3_1_5, AC_3_1_6, AC_3_1_7, AC_3_1_8, AC_3_1_9,
    AC_3_1_10, AC_3_1_11, AC_3_1_12, AC_3_1_13, AC_3_1_14, AC_3_1_15, AC_3_1_16, AC_3_1_17,
    AC_3_1_18, AC_3_1_19, AC_3_1_20, AC_3_1_21, AC_3_1_22, AT_3_2_1, AT_3_2_2, AT_3_2_3, AU_3_3_1,
    AU_3_3_2, AU_3_3_3, AU_3_3_4, AU_3_3_5, AU_3_3_6, AU_3_3_7, AU_3_3_8, AU_3_3_9, CM_3_4_1,
    CM_3_4_2, CM_3_4_3, CM_3_4_4, CM_3_4_5, CM_3_4_6, CM_3_4_7, CM_3_4_8, CM_3_4_9, IA_3_5_1,
    IA_3_5_2, IA_3_5_3, IA_3_5_4, IA_3_5_5, IA_3_5_6, IA_3_5_7, IA_3_5_8, IA_3_5_9, IA_3_5_10,
    IA_3_5_11, IR_3_6_1, IR_3_6_2, IR_3_6_3, MA_3_7_1, MA_3_7_2, MA_3_7_3, MA_3_7_4, MA_3_7_5,
    MA_3_7_6, MP_3_8_1, MP_3_8_2, MP_3_8_3, MP_3_8_4, MP_3_8_5, MP_3_8_6, MP_3_8_7, MP_3_8_8,
    MP_3_8_9, PS_3_9_1, PS_3_9_2, PE_3_10_1, PE_3_10_2, PE_3_10_3, PE_3_10_4, PE_3_10_5, PE_3_10_6,
    RA_3_11_1, RA_3_11_2, RA_3_11_3, CA_3_12_1, CA_3_12_2, CA_3_12_3, CA_3_12_4, SC_3_13_1,
    SC_3_13_2, SC_3_13_3, SC_3_13_4, SC_3_13_5, SC_3_13_6, SC_3_13_7, SC_3_13_8, SC_3_13_9,
    SC_3_13_10, SC_3_13_11, SC_3_13_12, SC_3_13_13, SC_3_13_14, SC_3_13_15, SC_3_13_16, SI_3_14_1,
    SI_3_14_2, SI_3_14_3, SI_3_14_4, SI_3_14_5, SI_3_14_6, SI_3_14_7,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s345_nist_has_exactly_110_controls() {
        assert_eq!(ALL_CONTROLS.len(), 110);
    }

    #[test]
    fn s345_nist_every_control_string_starts_with_its_dotted_id() {
        // Each constant is "<dotted-id>: <title>"; the dotted id must be the
        // "3.X.Y" prefix before the colon.
        for c in ALL_CONTROLS {
            let dotted = c.split(':').next().expect("has a colon-separated id");
            assert!(
                dotted.starts_with("3.") && dotted.matches('.').count() == 2,
                "control label is not a dotted 3.X.Y id: {c:?}"
            );
        }
    }

    #[test]
    fn s345_nist_all_controls_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for c in ALL_CONTROLS {
            assert!(seen.insert(c), "duplicate control entry: {c:?}");
        }
        assert_eq!(seen.len(), 110);
    }

    #[test]
    fn s345_nist_ac_3_1_1_matches_expected_label() {
        assert_eq!(
            AC_3_1_1,
            "3.1.1: Limit system access to authorized users and devices"
        );
    }
}
