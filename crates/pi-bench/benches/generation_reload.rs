use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pi_bench::{BenchConfig, BenchResult, BenchmarkReport, fixture_hash, measure, parameter_map};
use pi_core::{AgentPlugin, PluginId};
use pi_runtime::{PiRuntime, PiRuntimeBuilder};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};

fn main() -> BenchResult<()> {
    let config = BenchConfig::from_env(10, 50)?;
    let tokio = tokio::runtime::Runtime::new()?;
    let mut report = BenchmarkReport::new("generation_reload");

    let runtime = runtime_with_factories(16, None)?;
    report.push(measure(
        "reload_success_16_factories",
        config,
        fixture_hash("generation:v1:success:16"),
        parameter_map(&[("agent_plugin_factories", 16), ("provider_factories", 1)]),
        || {
            let previous = runtime.generation();
            let reloaded = tokio
                .block_on(runtime.reload())
                .map_err(|error| error.to_string())?;
            if reloaded.previous_generation != previous || reloaded.generation != previous + 1 {
                return Err("reload did not publish exactly one new generation".to_string());
            }
            black_box(reloaded);
            Ok(())
        },
    )?);

    let fail = Arc::new(AtomicBool::new(false));
    let failing_runtime = runtime_with_factories(16, Some(Arc::clone(&fail)))?;
    fail.store(true, Ordering::Release);
    let retained_generation = failing_runtime.generation();
    report.push(measure(
        "reload_failure_retains_generation",
        config,
        fixture_hash("generation:v1:failure:16"),
        parameter_map(&[("agent_plugin_factories", 16), ("provider_factories", 1)]),
        || {
            if tokio.block_on(failing_runtime.reload()).is_ok() {
                return Err("failing factory unexpectedly reloaded".to_string());
            }
            if failing_runtime.generation() != retained_generation {
                return Err("failed reload changed the active generation".to_string());
            }
            Ok(())
        },
    )?);

    report.finish()
}

struct BenchPlugin {
    id: String,
}

#[pi_core::agent_plugin]
impl AgentPlugin for BenchPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(self.id.clone())
    }
}

fn runtime_with_factories(
    factory_count: usize,
    failure: Option<Arc<AtomicBool>>,
) -> Result<PiRuntime, pi_runtime::RuntimeError> {
    let stable_count = factory_count - usize::from(failure.is_some());
    let mut builder = PiRuntimeBuilder::new();
    for index in 0..stable_count {
        let id = format!("bench-plugin-{index}");
        builder = builder.agent_plugin_factory(move || BenchPlugin { id: id.clone() });
    }
    if let Some(failure) = failure {
        builder = builder.try_agent_plugin_factory(move || {
            if failure.load(Ordering::Acquire) {
                Err("intentional benchmark reload failure")
            } else {
                Ok(BenchPlugin {
                    id: "bench-failing-plugin".to_string(),
                })
            }
        });
    }
    builder
        .provider_plugin_factory(|| {
            ScriptedProviderPlugin::scripted(std::iter::empty::<ScriptedTurn>())
        })
        .build()
}
