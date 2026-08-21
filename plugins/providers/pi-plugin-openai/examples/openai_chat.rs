use pi_agent::AgentOptions;
use pi_core::{ContentBlock, Message, ModelId, ProviderId};
use pi_plugin_openai::{OpenAiConfig, OpenAiPlugin};
use pi_runtime::PiRuntime;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    let api_key = required_env("OPENAI_API_KEY")?;
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "请用一句话介绍你自己。".to_string());

    let mut config = OpenAiConfig::new(api_key);
    if let Ok(base_url) = std::env::var("OPENAI_BASE_URL") {
        config = config.base_url(base_url);
    }
    if let Ok(organization) = std::env::var("OPENAI_ORGANIZATION") {
        config = config.organization(organization);
    }
    if let Ok(project) = std::env::var("OPENAI_PROJECT") {
        config = config.project(project);
    }

    let runtime = PiRuntime::builder()
        .provider_plugin(OpenAiPlugin::new(config)?)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("openai"),
            model_id: ModelId::new(model),
            system_prompt: "You are a concise assistant.".to_string(),
            ..AgentOptions::default()
        })
        .build()?;

    let outcome = runtime.prompt(prompt).await?;
    eprintln!("[stop] {:?}", outcome.stop);
    for message in outcome.new_messages {
        if let Message::Assistant(message) = message {
            if let Some(error) = &message.error_message {
                eprintln!("[provider error] {error}");
            }
            for block in &message.content {
                match block {
                    ContentBlock::Thinking(thinking) => {
                        eprintln!("[thinking] {}", thinking.thinking);
                    }
                    ContentBlock::Text(text) => print!("{}", text.text),
                    ContentBlock::Image(image) => println!("[image: {}]", image.mime_type),
                    ContentBlock::ToolCall(call) => {
                        println!("[tool call] {} {}", call.name, call.arguments);
                    }
                }
            }
        }
    }
    println!();
    Ok(())
}

fn required_env(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    std::env::var(name)
        .map_err(|_| format!("missing {name}; add it to the workspace .env file").into())
}
