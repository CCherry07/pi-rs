//! Learning-policy clauses from Hermes e629c900's background_review.py, with
//! Pi tool/ownership adaptations. Observe the actual Session -> Provider request;
//! do not compare a prompt builder with its own constants or simulate LLM learning.

use super::*;

fn final_user_prompt(request: &pi_core::ProviderRequest) -> String {
    let Some(Message::User(review)) = request.messages.last() else {
        panic!("the private review must append a user request");
    };
    review
        .content
        .iter()
        .filter_map(|block| match block {
            pi_core::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

async fn automatic_review_prompt(memory: bool, skills: bool) -> String {
    let root = tempfile::tempdir().unwrap();
    let plugin = plugin(
        root.path(),
        HermesMemoryConfig {
            nudge_interval: u64::from(memory),
            skill_nudge_interval: u64::from(skills),
            ..HermesMemoryConfig::default()
        },
    );
    let (session, provider) = session(
        root.path(),
        plugin.clone(),
        vec![
            ScriptedTurn::Text("Understood.".into()),
            ScriptedTurn::Text("Nothing to save.".into()),
        ],
    )
    .await;
    session
        .prompt("Please keep Rust code-review replies concise.")
        .await
        .unwrap();
    settled(&plugin).await;
    let requests = provider.requests();
    assert_eq!(requests.len(), 2, "one foreground request and one review");
    assert_eq!(requests[0].system_prompt, requests[1].system_prompt);
    assert_eq!(requests[0].tools, requests[1].tools);
    let prompt = final_user_prompt(&requests[1]);
    session.shutdown().await;
    prompt
}

#[tokio::test]
async fn lifecycle_flush_sends_the_complete_combined_learning_policy() {
    let root = tempfile::tempdir().unwrap();
    let plugin = plugin(
        root.path(),
        HermesMemoryConfig {
            nudge_interval: 0,
            skill_nudge_interval: 0,
            flush_on_shutdown: true,
            flush_min_turns: 1,
            ..HermesMemoryConfig::default()
        },
    );
    let (session, provider) = session(
        root.path(),
        plugin,
        vec![
            ScriptedTurn::Text("Foreground complete.".into()),
            text_with_usage(
                "Nothing to save.",
                Usage {
                    input: 21,
                    output: 3,
                    cache_read: 8,
                    cache_write: 2,
                    total_tokens: 34,
                    cost: UsageCost {
                        total: 0.42,
                        ..UsageCost::default()
                    },
                    ..Usage::default()
                },
            ),
        ],
    )
    .await;
    session.prompt("Finish this task.").await.unwrap();
    session.shutdown().await;

    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    let prompt = final_user_prompt(&requests[1]);
    for clause in [
        "**Memory**",
        "**Skills**",
        "FIRST-CLASS skill signals",
        "1. UPDATE A CURRENTLY-LOADED SKILL",
        "Unresolved failures",
        "Act on whichever of the two dimensions has real signal",
    ] {
        assert!(prompt.contains(clause), "flush omitted policy: {clause}");
    }
    let document = session.log().load().unwrap();
    let (review_usage, details) = document
        .records
        .iter()
        .find_map(|record| match &record.record {
            pi_session::LaneRecordEntry::Usage(usage)
                if matches!(
                    &usage.attribution,
                    pi_session::UsageAttribution::Adjustment {
                        details: Some(details),
                        ..
                    } if details.get("task") == Some(&json!("background_review"))
                ) =>
            {
                let pi_session::UsageAttribution::Adjustment {
                    details: Some(details),
                    ..
                } = &usage.attribution
                else {
                    unreachable!()
                };
                Some((&usage.usage, details))
            }
            _ => None,
        })
        .expect("shutdown review usage must be attributed to the parent session");
    assert_eq!(review_usage.input, 21);
    assert_eq!(review_usage.output, 3);
    assert_eq!(review_usage.cache_read, 8);
    assert_eq!(review_usage.cache_write, 2);
    assert!((review_usage.cost.total - 0.42).abs() < f64::EPSILON);
    assert_eq!(details["apiCalls"], 1);
}

#[tokio::test]
async fn unavailable_review_model_falls_back_to_the_live_session_model() {
    let root = tempfile::tempdir().unwrap();
    let plugin = plugin(
        root.path(),
        HermesMemoryConfig {
            nudge_interval: 1,
            skill_nudge_interval: 0,
            llm_model_override: Some("missing/review".into()),
            ..HermesMemoryConfig::default()
        },
    );
    let (session, provider) = session(
        root.path(),
        plugin.clone(),
        vec![
            ScriptedTurn::Text("Foreground complete.".into()),
            ScriptedTurn::Text("Nothing to save.".into()),
        ],
    )
    .await;
    session.prompt("Remember this preference.").await.unwrap();
    settled(&plugin).await;

    let requests = provider.requests();
    assert_eq!(
        requests.len(),
        2,
        "invalid optional model suppressed review"
    );
    assert_eq!(requests[1].model, requests[0].model);
    assert_eq!(requests[1].thinking_level, requests[0].thinking_level);
    assert_eq!(requests[1].thinking_budgets, requests[0].thinking_budgets);
    session.shutdown().await;
}

#[tokio::test]
async fn automatic_review_sends_mode_specific_hermes_learning_policy_to_provider() {
    for (memory, skills) in [(false, true), (true, false), (true, true)] {
        let prompt = automatic_review_prompt(memory, skills).await;
        if skills {
            assert!(
                prompt.contains("FIRST-CLASS skill signals"),
                "user corrections must be treated as skill-learning signals"
            );
        }
        assert_eq!(prompt.contains("**Memory**"), memory);
        assert_eq!(prompt.contains("**Skills**"), skills);
        assert!(prompt.contains("'Nothing to save.'"));
        if memory {
            for clause in [
                "persona, desires, preferences, or personal details",
                "expectations about how you should behave",
                "save facts about the user and durable preferences with the memory tool",
            ] {
                assert!(prompt.contains(clause), "missing memory policy: {clause}");
            }
        }
        if !skills {
            assert!(!prompt.contains("UPDATE A CURRENTLY-LOADED SKILL"));
            continue;
        }
        for clause in [
            "Be ACTIVE",
            "CLASS-LEVEL skills",
            "style, tone, format, legibility, or verbosity",
            "workflow, approach, or sequence of steps",
            "wrong, missing a step, or outdated",
            "the update belongs in the SKILL.md body, not just in memory",
            "references/<topic>.md",
            "templates/<name>.<ext>",
            "scripts/<name>.<ext>",
            "one-line pointer",
            "PR number, error string, feature codename, library-alone name",
            "Content quoted earlier in the conversation transcript does NOT count",
            "a NEW supporting file needs no prior read",
            "retry the write once; do not loop",
            "Environment-dependent failures",
            "Negative claims about tools or features",
            "the lesson is the retry pattern, not the original failure",
            "One-off task narratives",
            "Unresolved failures",
            "never the dead ends, and never dressed up as best practice",
            "capture the FIX",
            "no corrections and produced no new technique",
            // Pi adaptations must describe tools and provenance we actually support.
            "/skill:<name>",
            "skill_manage action=write_file",
            "file_path and content",
            "agent-created, unpinned skills with an unchanged content hash",
            "Only foreground user action can change a pinned skill",
            "Do not change ownership metadata",
        ] {
            assert!(
                prompt.contains(clause),
                "missing skill policy (memory={memory}): {clause}"
            );
        }
        let mut previous = 0;
        for action in [
            "1. UPDATE A CURRENTLY-LOADED SKILL",
            "2. UPDATE AN EXISTING UMBRELLA",
            "3. ADD A SUPPORT FILE",
            "4. CREATE A NEW CLASS-LEVEL UMBRELLA",
        ] {
            let position = prompt.find(action).expect(action);
            assert!(position > previous, "wrong learning priority: {action}");
            previous = position;
        }
        assert!(!prompt.contains("hermes curator adopt"));
        assert!(!prompt.contains("file_content"));
        if memory {
            assert!(prompt.contains("Act on whichever of the two dimensions has real signal"));
            assert!(!prompt.contains("If nothing is worth saving, just say"));
        }
    }
}
