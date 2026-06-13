use settings_value::SettingsValue;

use super::*;

#[test]
fn osc52_default_is_deny() {
    assert_eq!(Osc52ClipboardAccess::default(), Osc52ClipboardAccess::Deny);
}

#[test]
fn osc52_deny_blocks_read_and_write() {
    let access = Osc52ClipboardAccess::Deny;
    assert!(!access.allows_read());
    assert!(!access.allows_write());
}

#[test]
fn osc52_write_only_allows_write_but_not_read() {
    let access = Osc52ClipboardAccess::WriteOnly;
    assert!(access.allows_write());
    assert!(!access.allows_read());
}

#[test]
fn osc52_read_write_allows_both() {
    let access = Osc52ClipboardAccess::ReadWrite;
    assert!(access.allows_read());
    assert!(access.allows_write());
}

#[test]
fn osc52_deserializes_all_variants_from_settings_value() {
    let deny = Osc52ClipboardAccess::from_file_value(&serde_json::json!("deny")).unwrap();
    assert_eq!(deny, Osc52ClipboardAccess::Deny);

    let write_only =
        Osc52ClipboardAccess::from_file_value(&serde_json::json!("write_only")).unwrap();
    assert_eq!(write_only, Osc52ClipboardAccess::WriteOnly);

    let read_write =
        Osc52ClipboardAccess::from_file_value(&serde_json::json!("read_write")).unwrap();
    assert_eq!(read_write, Osc52ClipboardAccess::ReadWrite);
}

#[test]
fn osc52_rejects_unknown_variant() {
    assert!(Osc52ClipboardAccess::from_file_value(&serde_json::json!("allow_all")).is_none());
}

#[test]
fn cmd_arrow_line_nav_resolves_correctly() {
    // LineEditing: always control bytes, regardless of agent.
    assert_eq!(
        CmdArrowLineNav::LineEditing.resolve(true, LineEdge::Start),
        CmdArrowResolution::ControlByte(0x01) // Ctrl-A / SOH
    );
    assert_eq!(
        CmdArrowLineNav::LineEditing.resolve(false, LineEdge::End),
        CmdArrowResolution::ControlByte(0x05) // Ctrl-E / ENQ
    );

    // HomeEnd: always Home/End escape path, regardless of agent.
    assert_eq!(
        CmdArrowLineNav::HomeEnd.resolve(false, LineEdge::Start),
        CmdArrowResolution::HomeEnd
    );
    assert_eq!(
        CmdArrowLineNav::HomeEnd.resolve(true, LineEdge::End),
        CmdArrowResolution::HomeEnd
    );

    // Auto: Home/End when a CLI agent owns the session, control bytes otherwise.
    assert_eq!(
        CmdArrowLineNav::Auto.resolve(true, LineEdge::Start),
        CmdArrowResolution::HomeEnd
    );
    assert_eq!(
        CmdArrowLineNav::Auto.resolve(false, LineEdge::Start),
        CmdArrowResolution::ControlByte(0x01)
    );
    assert_eq!(
        CmdArrowLineNav::Auto.resolve(false, LineEdge::End),
        CmdArrowResolution::ControlByte(0x05)
    );

    // Default is Auto.
    assert_eq!(CmdArrowLineNav::default(), CmdArrowLineNav::Auto);
}
