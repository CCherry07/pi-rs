mod catalog;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use pi_eval::{ArtifactStore, EvalExecutionOutcome, EvalRun, PiEvalHarness, summarize_comparisons};
use pi_js_plugin::JsPluginHost;
use pi_sdk::ProductConfig;

use crate::catalog::{EvalPlan, resolve_plan};

#[derive(Debug, Parser)]
#[command(name = "pi-eval", about = "Run model-backed pi-rs evaluations")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run one evaluation case or case family.
    Run {
        /// Case name: smoke, docs, coding/model, coding/provider, provider/native-plugin, coding/js-extension, or coding/native-plugin.
        #[arg(default_value = "smoke")]
        case: String,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
        /// Run only one named variant. Comparative cases run all variants by default.
        #[arg(long)]
        variant: Option<String>,
        #[arg(long, default_value_t = 1)]
        repetitions: u32,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        agent_dir: Option<PathBuf>,
        #[arg(long)]
        artifact_dir: Option<PathBuf>,
        /// Explicit JavaScript/TypeScript extension source. Requires the Node launcher.
        #[arg(long = "extension")]
        extensions: Vec<String>,
        /// Explicit native plugin library, pi-plugin.toml, or package directory.
        #[arg(long = "native-plugin")]
        native_plugins: Vec<PathBuf>,
    },
}

pub async fn run_from_env() -> Result<(), String> {
    dotenvy::dotenv().ok();
    execute(Cli::parse(), None).await
}

/// Runs the eval CLI while Node owns JavaScript/TypeScript extension loading.
/// `arguments` follows `process.argv.slice(2)` and excludes an executable name.
pub async fn run_with_js_host(
    arguments: Vec<String>,
    host: Arc<dyn JsPluginHost>,
) -> Result<(), String> {
    dotenvy::dotenv().ok();
    let cli = match Cli::try_parse_from(std::iter::once("pi-eval".to_string()).chain(arguments)) {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            print!("{error}");
            return Ok(());
        }
        Err(error) => return Err(error.to_string()),
    };
    execute(cli, Some(host)).await
}

async fn execute(cli: Cli, js_plugin_host: Option<Arc<dyn JsPluginHost>>) -> Result<(), String> {
    match cli.command {
        Command::Run {
            case,
            provider,
            model,
            variant,
            repetitions,
            base_url,
            agent_dir,
            artifact_dir,
            extensions,
            native_plugins,
        } => {
            if repetitions == 0 {
                return Err("--repetitions must be greater than zero".to_string());
            }
            let provider = resolve_selection(provider, "PI_PROVIDER", "--provider")?;
            let model = resolve_selection(model, "PI_MODEL", "--model")?;
            let source_agent_dir = agent_dir
                .or_else(pi_sdk::default_agent_dir)
                .ok_or_else(|| "cannot determine agent directory; pass --agent-dir".to_string())?;
            let cwd = std::env::current_dir()
                .map_err(|error| format!("cannot determine current directory: {error}"))?;
            let artifact_dir = artifact_dir.unwrap_or_else(default_artifact_directory);
            let artifact_store =
                ArtifactStore::new(&artifact_dir).map_err(|error| error.to_string())?;
            let mut harness = PiEvalHarness::new(artifact_store.clone());
            if let Some(host) = js_plugin_host {
                harness = harness.with_js_plugin_host(host);
            }
            let plan = select_variants(
                resolve_plan(&case)?,
                variant.as_deref(),
                &extensions,
                &native_plugins,
            )?;
            println!("[eval] provider={provider} model={model}");
            println!("[eval] artifacts={}", artifact_dir.display());
            let mut runs = Vec::new();
            let mut failures = 0_u32;
            for repetition in 1..=repetitions {
                for eval_case in &plan.cases {
                    for eval_variant in &plan.variants {
                        println!(
                            "[eval] case={} variant={} repetition={repetition}",
                            eval_case.id, eval_variant.name
                        );
                        let config = product_config(
                            &cwd,
                            &source_agent_dir,
                            &provider,
                            &model,
                            base_url.as_deref(),
                        );
                        let run = harness
                            .run(eval_case, eval_variant.clone(), repetition, config)
                            .await
                            .map_err(|error| error.to_string())?;
                        print_run(&run);
                        let failed = if plan.eval_set.is_some() {
                            run.execution_outcome != EvalExecutionOutcome::Completed
                        } else {
                            !run.passed
                        };
                        failures = failures.saturating_add(u32::from(failed));
                        runs.push(run);
                    }
                }
            }
            if let Some(definition) = plan.comparison_definition(repetitions) {
                let report = summarize_comparisons(&[definition], &runs)?;
                artifact_store
                    .persist_comparison_report(&report)
                    .map_err(|error| error.to_string())?;
                println!("{}", pi_eval::format_comparison_report(&report));
            }
            if failures > 0 {
                return Err(format!("{failures}/{} eval runs failed", runs.len()));
            }
            Ok(())
        }
    }
}

fn select_variants(
    mut plan: EvalPlan,
    selected: Option<&str>,
    extensions: &[String],
    native_plugins: &[PathBuf],
) -> Result<EvalPlan, String> {
    if let Some(selected) = selected {
        if plan.variants.len() == 1 {
            plan.variants[0].name = selected.to_string();
            plan.eval_set = None;
        } else {
            plan.variants.retain(|variant| variant.name == selected);
            if plan.variants.is_empty() {
                return Err(format!("unknown variant for this case: {selected}"));
            }
            plan.eval_set = None;
        }
    }
    for variant in &mut plan.variants {
        variant.extensions.extend(extensions.iter().cloned());
        variant
            .native_plugins
            .extend(native_plugins.iter().cloned());
    }
    Ok(plan)
}

fn product_config(
    cwd: &std::path::Path,
    source_agent_dir: &std::path::Path,
    provider: &str,
    model: &str,
    base_url: Option<&str>,
) -> ProductConfig {
    let mut config = ProductConfig::new(cwd.to_path_buf(), source_agent_dir.to_path_buf());
    config.provider = provider.to_string();
    config.requested_provider = Some(provider.to_string());
    config.model = Some(model.to_string());
    if provider != "openai-compatible" {
        // ProductConfig discovers OPENAI_API_KEY for its default provider. A
        // later eval provider selection must not reuse that credential.
        config.api_key = None;
    }
    if let Some(base_url) = base_url {
        config.base_url = base_url.to_string();
    }
    config
}

fn print_run(run: &EvalRun) {
    println!(
        "[eval] passed={} tokens={} latency={}ms run={}",
        run.passed, run.observation.usage.total_tokens, run.duration_ms, run.run_id
    );
    for grade in &run.grades {
        println!(
            "[eval] grader={} score={:.3} passed={} rationale={}",
            grade.grader, grade.score, grade.passed, grade.rationale
        );
    }
    for error in &run.observation.errors {
        eprintln!("[eval] error={error}");
    }
}

fn resolve_selection(
    explicit: Option<String>,
    environment_name: &str,
    flag: &str,
) -> Result<String, String> {
    explicit
        .or_else(|| std::env::var(environment_name).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("select {flag} or set {environment_name}"))
}

fn default_artifact_directory() -> PathBuf {
    if let Some(path) = std::env::var_os("PI_EVAL_ARTIFACT_DIR") {
        return PathBuf::from(path);
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    PathBuf::from(".eval").join(format!("{timestamp}-{}", uuid::Uuid::now_v7()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_plugin_sources_apply_to_every_comparison_variant() {
        let plan = select_variants(
            resolve_plan("js-extension").unwrap(),
            None,
            &["fixture.ts".to_string()],
            &[PathBuf::from("native/pi-plugin.toml")],
        )
        .unwrap();
        assert_eq!(plan.variants.len(), 2);
        assert!(plan.variants.iter().all(|variant| {
            variant.extensions == ["fixture.ts"]
                && variant.native_plugins == [PathBuf::from("native/pi-plugin.toml")]
        }));
    }

    #[test]
    fn selecting_one_comparison_variant_disables_incomplete_report() {
        let plan = select_variants(
            resolve_plan("js-extension").unwrap(),
            Some("default-system-prompt"),
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(plan.variants.len(), 1);
        assert!(plan.eval_set.is_none());
    }
}
