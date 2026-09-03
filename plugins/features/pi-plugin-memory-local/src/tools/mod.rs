//! Model-facing tools registered by the local memory provider.

mod memory;
mod output;
mod session_search;

use std::sync::Arc;

use pi_core::RegisterContext;

use self::memory::MemoryTool;
use self::session_search::SessionSearchTool;
use crate::runtime::LocalMemoryRuntime;

/// Registers the stable model-facing Interface owned by this provider.
///
/// Tool names remain semantic (`memory` and `session_search`); only the
/// user-facing maintenance commands carry the provider-specific prefix.
pub(crate) fn register(
    context: &mut RegisterContext<'_>,
    runtime: &LocalMemoryRuntime,
) -> pi_core::Result<()> {
    context.register_tool(Arc::new(MemoryTool::new(runtime.clone())))?;
    context.register_tool(Arc::new(SessionSearchTool::new(runtime.clone())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::Tool;

    #[test]
    fn tools_satisfy_the_plugin_registry_contract() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<MemoryTool>();
        assert_send_sync::<SessionSearchTool>();
        assert_send_sync::<Arc<dyn Tool>>();
    }
}
