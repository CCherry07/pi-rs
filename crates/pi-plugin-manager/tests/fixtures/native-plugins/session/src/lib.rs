use pi_plugin::prelude::*;

pub struct FixtureSessionPlugin;

#[pi_plugin::native_plugin(factory)]
impl Plugin for FixtureSessionPlugin {
    type Options = std::collections::BTreeMap<String, String>;

    fn prepare(context: &PrepareContext, options: Self::Options) -> PrepareResult<Option<Self>> {
        for (key, actual) in [
            ("expected_cwd", context.cwd().to_string_lossy().into_owned()),
            (
                "expected_project_trusted",
                context.project_trusted().to_string(),
            ),
            (
                "expected_roots",
                context.workspace().roots().len().to_string(),
            ),
        ] {
            if let Some(expected) = options.get(key)
                && actual != *expected
            {
                return Err(PrepareError::initialization(format!("incorrect {key}")));
            }
        }
        Ok(Some(Self))
    }

    async fn session_start(
        &self,
        _context: &SessionPluginContext,
        _event: &SessionStartEvent,
    ) -> std::result::Result<(), PluginError> {
        Ok(())
    }
}
