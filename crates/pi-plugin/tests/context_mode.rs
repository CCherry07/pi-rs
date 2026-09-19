use pi_plugin::prelude::*;

#[test]
fn isolated_context_types_are_available_in_the_author_prelude() {
    assert_eq!(
        IsolatedSessionOptions::default().context,
        IsolatedContextMode::Fresh
    );
    let point = IsolatedForkPoint {
        parent_session_id: "parent".into(),
        parent_entry_id: "entry".into(),
    };
    assert_eq!(point.parent_entry_id, "entry");
}
