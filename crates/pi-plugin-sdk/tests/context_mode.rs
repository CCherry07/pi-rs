#[test]
fn isolated_context_types_are_available_in_each_enabled_author_prelude() {
    #[cfg(feature = "agent")]
    {
        use pi_plugin_sdk::agent::prelude::*;
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
    #[cfg(feature = "provider")]
    {
        use pi_plugin_sdk::provider::prelude::*;
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
    #[cfg(feature = "session")]
    {
        use pi_plugin_sdk::session::prelude::*;
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
}
