use crate::UiCompletion;

/// Generation-local, ordered argument candidates supplied when a command is registered.
/// Values replace the complete argument string (not the command name). Trailing whitespace
/// is significant: it can move the composer on to the next argument. This is a finite,
/// whitespace-token completion catalog, not shell parsing or an executable callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandCompletions {
    pub name: String,
    pub items: Vec<UiCompletion>,
}

impl CommandCompletions {
    /// Match the current positional argument, preserving the owner's candidate order.
    /// Complete preceding words must match exactly; the current word matches by prefix.
    pub fn matching(&self, prefix: &str) -> Vec<&UiCompletion> {
        let mut words = prefix.split_whitespace().collect::<Vec<_>>();
        if words.is_empty() || prefix.ends_with(char::is_whitespace) {
            words.push("");
        }
        self.items
            .iter()
            .filter(|item| {
                let candidate = item.value.split_whitespace().collect::<Vec<_>>();
                candidate.len() == words.len()
                    && candidate
                        .iter()
                        .zip(&words)
                        .enumerate()
                        .all(|(index, (value, word))| {
                            if index + 1 == words.len() {
                                value.starts_with(*word)
                            } else {
                                value == word
                            }
                        })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Command, CommandContext, CommandError, CommandOutcome, CommandSpec, PluginId,
        RegistriesBuilder,
    };
    use std::sync::Arc;

    fn candidate(value: &str) -> UiCompletion {
        UiCompletion {
            value: value.into(),
            label: value.trim().into(),
            description: None,
        }
    }

    #[test]
    fn positional_matching_preserves_order_unicode_and_continuation_spaces() {
        let completions = CommandCompletions {
            name: "custom".into(),
            items: ["view ", "visit ", "view 東京", "view 台北", "visit local"]
                .into_iter()
                .map(candidate)
                .collect(),
        };
        for (prefix, expected) in [
            ("", vec!["view ", "visit "]),
            ("  vi", vec!["view ", "visit "]),
            ("view ", vec!["view 東京", "view 台北"]),
            (" view\t東", vec!["view 東京"]),
            ("view 東京 ", vec![]),
            ("unknown", vec![]),
        ] {
            assert_eq!(
                completions
                    .matching(prefix)
                    .iter()
                    .map(|item| item.value.as_str())
                    .collect::<Vec<_>>(),
                expected,
                "{prefix:?}"
            );
        }
    }

    struct Fixture(&'static str);
    #[async_trait::async_trait]
    impl Command for Fixture {
        fn spec(&self) -> CommandSpec {
            CommandSpec {
                name: self.0.into(),
                description: String::new(),
                argument_hint: None,
            }
        }
        async fn execute(
            &self,
            _: CommandContext,
            _: String,
        ) -> Result<CommandOutcome, CommandError> {
            panic!("reading completion metadata must never execute a command")
        }
    }

    #[test]
    fn completion_registration_is_atomic_ordered_and_scoped_with_commands() {
        let mut builder = RegistriesBuilder::new();
        builder
            .register_command_with_completions(
                PluginId::new("owner"),
                Arc::new(Fixture("z-last-alphabetically")),
                vec![candidate("second "), candidate("first ")],
            )
            .unwrap();
        builder
            .register_command_with_completions(
                PluginId::new("owner"),
                Arc::new(Fixture("a-first-alphabetically")),
                vec![candidate("other")],
            )
            .unwrap();
        assert!(
            builder
                .register_command_with_completions(
                    PluginId::new("other-owner"),
                    Arc::new(Fixture("z-last-alphabetically")),
                    vec![candidate("overwrite")]
                )
                .is_err()
        );
        builder
            .register_command(PluginId::new("owner"), Arc::new(Fixture("without")))
            .unwrap();
        let registry = builder.freeze();
        let mut snapshot = registry.command_completions();
        assert_eq!(
            snapshot
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["z-last-alphabetically", "a-first-alphabetically"]
        );
        assert_eq!(
            snapshot[0]
                .items
                .iter()
                .map(|item| item.value.as_str())
                .collect::<Vec<_>>(),
            ["second ", "first "]
        );
        snapshot[0].items.clear();
        assert_eq!(registry.command_completions()[0].items.len(), 2);
        assert_eq!(
            registry.command_owner("z-last-alphabetically"),
            Some(&PluginId::new("owner"))
        );
        assert_eq!(
            registry.with_allowed_tools(&[]).command_completions(),
            registry.command_completions()
        );
        assert!(
            registry
                .restrict_tools(&[])
                .unwrap()
                .command_completions()
                .is_empty()
        );
        assert!(
            registry
                .scope_tool_execution(&[], "fixture")
                .command_completions()
                .is_empty()
        );
    }
}
