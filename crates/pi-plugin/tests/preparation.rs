use std::sync::Arc;

use pi_plugin::prelude::*;

struct ConfiguredPlugin(String);

#[pi_plugin::plugin]
impl Plugin for ConfiguredPlugin {
    type Options = String;

    fn prepare(_: &PrepareContext, options: Self::Options) -> PrepareResult<Option<Self>> {
        Ok(Some(Self(options)))
    }

    fn id(&self) -> PluginId {
        PluginId::new(&self.0)
    }
}

struct HostPlugin;

#[pi_plugin::plugin]
impl Plugin for HostPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("host")
    }
}

struct ConfiguredProviderPlugin(String);

#[pi_plugin::provider_plugin]
impl ProviderPlugin for ConfiguredProviderPlugin {
    type Options = String;

    fn prepare(_: &PrepareContext, options: Self::Options) -> PrepareResult<Option<Self>> {
        Ok(Some(Self(options)))
    }

    fn id(&self) -> PluginId {
        PluginId::new(&self.0)
    }
}

struct HostProviderPlugin;

#[pi_plugin::provider_plugin]
impl ProviderPlugin for HostProviderPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("host-provider")
    }
}

fn context() -> PrepareContext {
    PrepareContext::new(WorkspaceSpec::from_cwd(".").snapshot(), "./agent", false)
}

#[test]
fn typed_construction_and_mixed_trait_objects_share_the_plugin_contracts() {
    fn prepare<P: Plugin>(options: P::Options) -> P {
        P::prepare(&context(), options).unwrap().unwrap()
    }
    fn prepare_provider<P: ProviderPlugin>(options: P::Options) -> P {
        P::prepare(&context(), options).unwrap().unwrap()
    }

    let plugins: Vec<Arc<dyn Plugin>> = vec![
        Arc::new(prepare::<ConfiguredPlugin>("configured".into())),
        Arc::new(HostPlugin),
    ];
    let driver = PluginDriver::new(plugins).unwrap();
    assert_eq!(driver.plugin_order(), ["configured".into(), "host".into()]);

    let providers: Vec<Arc<dyn ProviderPlugin>> = vec![
        Arc::new(prepare_provider::<ConfiguredProviderPlugin>(
            "configured-provider".into(),
        )),
        Arc::new(HostProviderPlugin),
    ];
    let driver = ProviderPluginDriver::new(providers).unwrap();
    assert_eq!(
        driver.plugin_order(),
        ["configured-provider".into(), "host-provider".into()]
    );
}

#[test]
fn host_injected_plugins_reject_typed_preparation_instead_of_disabling_silently() {
    let plugin = HostPlugin::prepare(&context(), ());
    assert!(matches!(plugin, Err(PrepareError::Initialization(message))
        if message.contains("host-provided factory") && message.contains("Plugin::prepare")));
    let provider = HostProviderPlugin::prepare(&context(), ());
    assert!(
        matches!(provider, Err(PrepareError::Initialization(message))
        if message.contains("host-provided factory") && message.contains("ProviderPlugin::prepare"))
    );
}
