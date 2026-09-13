//! One launch policy for both single children and workflow nodes. A batch pins
//! its fork source on first use, before any child or parent continuation runs.
use pi_core::{
    IsolatedContextMode, IsolatedForkPoint, IsolatedSessionOptions, ToolContext, ToolError,
};
use serde::{Deserialize, Deserializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForkTurns {
    None,
    All,
    Recent(usize),
}

impl<'de> Deserialize<'de> for ForkTurns {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "none" => Ok(Self::None),
            "all" => Ok(Self::All),
            _ => value
                .parse::<usize>()
                .ok()
                .filter(|turns| *turns > 0)
                .map(Self::Recent)
                .ok_or_else(|| {
                    serde::de::Error::custom(
                        "fork_turns must be \"none\", \"all\", or a positive integer string",
                    )
                }),
        }
    }
}

pub(crate) struct LaunchContext<'a> {
    context: &'a ToolContext,
    fork_point: Option<Option<IsolatedForkPoint>>,
}

impl<'a> LaunchContext<'a> {
    pub fn new(context: &'a ToolContext) -> Self {
        Self {
            context,
            fork_point: None,
        }
    }

    pub fn apply(
        &mut self,
        options: &mut IsolatedSessionOptions,
        explicit: Option<ForkTurns>,
    ) -> Result<(), ToolError> {
        match explicit {
            Some(ForkTurns::None) => {
                options.context = IsolatedContextMode::Fresh;
                options.fork_turns = None;
            }
            Some(ForkTurns::All) => {
                options.context = IsolatedContextMode::Fork;
                options.fork_turns = None;
            }
            Some(ForkTurns::Recent(turns)) => {
                options.context = IsolatedContextMode::Fork;
                options.fork_turns = Some(turns);
            }
            None => {}
        }
        options.fork_point = None;
        if options.context == IsolatedContextMode::Fresh {
            return Ok(());
        }
        if self.fork_point.is_none() {
            self.fork_point = Some(self.context.session.isolated_fork_point()?);
        }
        match self.fork_point.as_ref().expect("captured fork point") {
            Some(fork_point) => options.fork_point = Some(fork_point.clone()),
            None if explicit.is_none() => {
                options.context = IsolatedContextMode::Fresh;
                options.fork_turns = None;
            }
            None => return Err(ToolError::Execution(
                "fork_turns all or a positive number requires a persisted parent branch; wait for the first assistant response or use fork_turns none.".into(),
            )),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{ModelsContextAccess, SessionContextAccess, UiContextAccess};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct Access {
        available: bool,
        reads: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl SessionContextAccess for Access {
        fn isolated_fork_point(&self) -> pi_core::PluginContextResult<Option<IsolatedForkPoint>> {
            let index = self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(self.available.then(|| IsolatedForkPoint {
                parent_session_id: "parent".into(),
                parent_entry_id: format!("entry-{index}"),
            }))
        }
    }
    #[async_trait::async_trait]
    impl ModelsContextAccess for Access {}
    #[async_trait::async_trait]
    impl UiContextAccess for Access {}

    #[test]
    fn defaults_fallback_explicit_fork_is_strict_and_batch_captures_once() {
        for available in [false, true] {
            let access = Arc::new(Access {
                available,
                reads: AtomicUsize::new(0),
            });
            let epoch = pi_core::PluginContextEpoch::new(access.clone());
            let (_, signal) = pi_core::AbortHandle::new();
            let context = ToolContext::with_plugin_context(".".into(), signal, epoch.context());
            let mut batch = LaunchContext::new(&context);
            let mut fresh = IsolatedSessionOptions::default();
            batch.apply(&mut fresh, None).unwrap();
            assert_eq!(access.reads.load(Ordering::SeqCst), 0);
            let mut implicit = IsolatedSessionOptions {
                context: IsolatedContextMode::Fork,
                ..Default::default()
            };
            batch.apply(&mut implicit, None).unwrap();
            assert_eq!(
                implicit.context,
                if available {
                    IsolatedContextMode::Fork
                } else {
                    IsolatedContextMode::Fresh
                }
            );
            let mut explicit = IsolatedSessionOptions::default();
            assert_eq!(
                batch.apply(&mut explicit, Some(ForkTurns::All)).is_ok(),
                available
            );
            assert_eq!(implicit.fork_point, explicit.fork_point);
            let mut recent = IsolatedSessionOptions::default();
            assert_eq!(
                batch.apply(&mut recent, Some(ForkTurns::Recent(3))).is_ok(),
                available
            );
            assert_eq!(recent.fork_turns, Some(3));
            assert_eq!(implicit.fork_point, recent.fork_point);
            let mut none = IsolatedSessionOptions {
                context: IsolatedContextMode::Fork,
                fork_turns: Some(2),
                ..Default::default()
            };
            batch.apply(&mut none, Some(ForkTurns::None)).unwrap();
            assert_eq!(none.context, IsolatedContextMode::Fresh);
            assert_eq!(none.fork_turns, None);
            assert_eq!(access.reads.load(Ordering::SeqCst), 1);
        }
    }
}
