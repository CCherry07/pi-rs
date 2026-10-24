use pi_plugin::UiCommand;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Case {
    name: String,
    value: Value,
}

#[derive(Deserialize)]
struct Fixtures {
    valid: Vec<Case>,
    invalid: Vec<Case>,
}

#[test]
fn commands_match_the_shared_rust_javascript_wire_corpus() {
    let fixtures: Fixtures =
        serde_json::from_str(include_str!("fixtures/ui-commands.json")).unwrap();
    for case in fixtures.valid {
        let command: UiCommand = serde_json::from_value(case.value.clone())
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        assert_eq!(
            serde_json::to_value(command).unwrap(),
            case.value,
            "{}",
            case.name
        );
    }
    for case in fixtures.invalid {
        assert!(
            serde_json::from_value::<UiCommand>(case.value).is_err(),
            "{}",
            case.name
        );
    }
}
