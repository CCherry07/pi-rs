#[test]
fn isolated_context_mode_is_available_in_each_enabled_author_prelude() {
    #[cfg(feature = "agent")]
    {
        use pi_plugin_sdk::agent::prelude::*;
        assert_eq!(
            IsolatedSessionOptions::default().context,
            IsolatedContextMode::Fresh
        );
    }
    #[cfg(feature = "provider")]
    {
        use pi_plugin_sdk::provider::prelude::*;
        assert_eq!(
            IsolatedSessionOptions::default().context,
            IsolatedContextMode::Fresh
        );
    }
    #[cfg(feature = "session")]
    {
        use pi_plugin_sdk::session::prelude::*;
        assert_eq!(
            IsolatedSessionOptions::default().context,
            IsolatedContextMode::Fresh
        );
    }
}
