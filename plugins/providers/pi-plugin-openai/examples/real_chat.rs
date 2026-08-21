use pi_agent::AgentOptions;
use pi_core::{ContentBlock, Message, ModelId, ProviderId};
use pi_plugin_openai::{OpenAiCompatibleConfig, OpenAiCompatiblePlugin};
use pi_runtime::PiRuntime;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Loads /Users/cherry/Documents/pi_rs/.env when run from the workspace.
    dotenvy::dotenv().ok();

    let api_key = required_env("OPENAI_API_KEY")?;
    let base_url = std::env::var("OPENAI_BASE_URL")
        .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
    let model = required_env("OPENAI_MODEL")?;
    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "请用一句话介绍你自己。".to_string());

    let provider_id = ProviderId::new("openai-compatible");
    let plugin = OpenAiCompatiblePlugin::new(OpenAiCompatibleConfig::new(base_url, api_key))?;
    let runtime = PiRuntime::builder()
        .provider_plugin(plugin)
        .agent_options(AgentOptions {
            provider_id,
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
