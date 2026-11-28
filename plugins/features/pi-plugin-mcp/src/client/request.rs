//! Logical operations over frozen callable views and exact connection owners.
use super::*;
use crate::http_client::HttpError;
use crate::supervisor::{
    ConnectionAdmission, OwnedConnectionSlot, WeakConnectionSlot, WeakMcpGenerationReadiness,
};
use operation::{LogicalOperation, OperationError, OperationGuard, RequestControl};
use rmcp::service::{RequestHandle, ServiceError};

#[cfg(test)]
mod tests;

pub(super) struct CallableGrant {
    remote: McpToolSpec,
    spec: ToolSpec,
    policy: McpServerPolicy,
}

pub(super) struct ConnectionBinding {
    ownership: ConnectionOwnership,
    grants: BTreeMap<String, CallableGrant>,
}

enum ConnectionOwnership {
    Supervised {
        authority: WeakMcpGenerationReadiness,
        slot: WeakConnectionSlot,
    },
    Explicit(OwnedConnectionSlot),
}

impl ConnectionBinding {
    fn authority(&self) -> Option<WeakMcpGenerationReadiness> {
        match &self.ownership {
            ConnectionOwnership::Supervised { authority, .. } => Some(authority.clone()),
            ConnectionOwnership::Explicit(_) => None,
        }
    }

    async fn acquire(&self) -> Result<ConnectionAdmission, McpRequestError> {
        match &self.ownership {
            ConnectionOwnership::Supervised { slot, .. } => {
                slot.acquire().await.map_err(McpRequestError::Control)
            }
            ConnectionOwnership::Explicit(owner) => match owner.acquire().await {
                Ok(admission) => Ok(admission),
                Err(crate::supervisor::AdmissionError::Control(error)) => {
                    Err(McpRequestError::Control(error))
                }
                Err(crate::supervisor::AdmissionError::Closed(connection)) => {
                    connection.handler.stderr.finish().await;
                    Err(McpRequestError::Disconnected(
                        connection.handler.stderr.message("Connection closed"),
                    ))
                }
            },
        }
    }

    fn supervised_slot(&self) -> Option<&WeakConnectionSlot> {
        match &self.ownership {
            ConnectionOwnership::Supervised { slot, .. } => Some(slot),
            ConnectionOwnership::Explicit(_) => None,
        }
    }

    fn slot(&self) -> WeakConnectionSlot {
        match &self.ownership {
            ConnectionOwnership::Supervised { slot, .. } => slot.clone(),
            ConnectionOwnership::Explicit(owner) => owner.downgrade(),
        }
    }

    pub(super) fn owned(&self) -> Option<&OwnedConnectionSlot> {
        match &self.ownership {
            ConnectionOwnership::Explicit(owner) => Some(owner),
            _ => None,
        }
    }

    pub(super) fn diagnostic(&self) -> Option<String> {
        use crate::supervisor::ConnectionReadiness;
        match self.slot().readiness() {
            ConnectionReadiness::Ready(_) | ConnectionReadiness::Connecting => None,
            ConnectionReadiness::Disconnected(inventory) => {
                // Observe the exact current slot, not the frozen view's original transport.
                // Its existing capture already enforces redaction and diagnostic bounds.
                Some(
                    inventory
                        .connection
                        .handler
                        .stderr
                        .message("Connection closed"),
                )
            }
            ConnectionReadiness::Failed(error) => Some(error.to_string()),
            ConnectionReadiness::Retired => Some("Connection closed".into()),
        }
    }
}

impl McpToolSet {
    /// Attach only after the final naming/quarantine pass. Grants are immutable executable-view
    /// data; raw supervisor slots never own or infer policy from names.
    pub(crate) fn with_supervision(
        &self,
        authority: WeakMcpGenerationReadiness,
    ) -> Result<Self, OperationError> {
        self.bind_connections(|old| {
            Ok(ConnectionOwnership::Supervised {
                slot: authority
                    .connection_slot(&old.name)
                    .ok_or(OperationError::AuthorityRetired)?,
                authority: authority.clone(),
            })
        })
    }

    pub(super) fn with_explicit_connections(
        &self,
        mut owners: BTreeMap<String, OwnedConnectionSlot>,
    ) -> Result<Self, McpError> {
        self.bind_connections(|old| {
            owners
                .remove(&old.name)
                .map(ConnectionOwnership::Explicit)
                .ok_or_else(|| McpError::Initialize {
                    server: old.name.clone(),
                    message: "connection owner is unavailable".into(),
                })
        })
    }

    fn bind_connections<E>(
        &self,
        mut ownership: impl FnMut(&ConnectedServer) -> Result<ConnectionOwnership, E>,
    ) -> Result<Self, E> {
        let mut servers = Vec::new();
        let mut tools = Vec::new();
        for old in &self.inner.servers {
            let grants = self
                .inner
                .tools
                .iter()
                .filter(|tool| Arc::ptr_eq(&tool.server, old))
                .map(|tool| {
                    (
                        tool.remote_name.clone(),
                        CallableGrant {
                            remote: tool.frozen_remote.clone(),
                            spec: tool.spec(),
                            policy: old.policy.clone(),
                        },
                    )
                })
                .collect();
            let server = Arc::new(ConnectedServer {
                name: old.name.clone(),
                policy: old.policy.clone(),
                connection: old.connection.clone(),
                binding: Some(ConnectionBinding {
                    ownership: ownership(old)?,
                    grants,
                }),
            });
            tools.extend(
                self.inner
                    .tools
                    .iter()
                    .filter(|tool| Arc::ptr_eq(&tool.server, old))
                    .map(|tool| {
                        Arc::new(McpTool::new(
                            server.clone(),
                            tool.frozen_remote.clone(),
                            tool.descriptor.name.clone(),
                        ))
                    }),
            );
            servers.push(server);
        }
        let resource_tools = create_resource_tools(
            servers
                .iter()
                .filter(|server| server.resources_visible())
                .cloned()
                .collect(),
        );
        Ok(Self {
            inner: Arc::new(McpPool {
                inventory: self.inner.inventory.clone(),
                authorization_required: self.inner.authorization_required.clone(),
                servers,
                tools,
                resource_tools,
            }),
        })
    }
}

pub(crate) struct RequestOperation<'a> {
    server: &'a ConnectedServer,
    context: &'a ToolContext,
    guard: OperationGuard,
    admission: Option<ConnectionAdmission>,
    deadline: tokio::time::Instant,
}

impl ConnectedServer {
    pub(crate) fn operation<'a>(&'a self, context: &'a ToolContext) -> RequestOperation<'a> {
        RequestOperation {
            server: self,
            context,
            guard: OperationGuard::new(
                context.signal().clone(),
                self.binding.as_ref().and_then(ConnectionBinding::authority),
            ),
            admission: None,
            deadline: tokio::time::Instant::now() + self.policy.timeout,
        }
    }

    pub(crate) async fn request(
        &self,
        context: &ToolContext,
        request: ClientRequest,
        tool: &str,
        updates: &ToolUpdateSink,
    ) -> Result<ServerResult, McpRequestError> {
        let resource = matches!(
            request,
            ClientRequest::ReadResourceRequest(_)
                | ClientRequest::ListResourcesRequest(_)
                | ClientRequest::ListResourceTemplatesRequest(_)
        );
        let mut operation = self.operation(context);
        loop {
            match operation.request(request.clone(), tool, updates).await {
                Ok(result) => return Ok(result),
                Err(error) => {
                    if !operation.recover(&error, resource).await? {
                        return Err(error);
                    }
                }
            }
        }
    }
}

impl RequestOperation<'_> {
    fn control(&self) -> LogicalOperation {
        self.guard.operation()
    }

    fn host_tools(&self) -> Result<Option<Vec<ToolSpec>>, McpRequestError> {
        if self.context.run_id().is_none() {
            return Ok(None);
        }
        match self.context.session.tools() {
            Ok(specs) => Ok(Some(specs)),
            // Ephemeral Agents deliberately have no parent-session capabilities. Their
            // ScopedTool has already admitted this exact callable; missing optional host
            // inventory is not retirement and cannot substitute for that dispatch grant.
            // The generation lease, transport gate and run cancellation still apply.
            Err(pi_plugin::PluginContextError::Unbound) => Ok(None),
            Err(_) => Err(McpRequestError::Control(OperationError::AuthorityRetired)),
        }
    }

    async fn connection(
        &mut self,
        request: &ClientRequest,
    ) -> Result<Arc<McpConnection>, McpRequestError> {
        let control = self.control();
        control.check().map_err(McpRequestError::Control)?;
        let Some(view) = &self.server.binding else {
            return Ok(self.server.connection.clone());
        };
        // A retired host context cannot start shared setup, even for a resource helper.
        drop(self.host_tools()?);
        if self.admission.is_none() {
            self.admission = Some(tokio::select! {
                biased;
                () = self.context.signal().wait() => return Err(McpRequestError::Aborted),
                () = control.wait_retired() => return Err(McpRequestError::Control(OperationError::AuthorityRetired)),
                () = tokio::time::sleep_until(self.deadline) => return Err(McpRequestError::Timeout(self.server.policy.timeout)),
                result = view.acquire() => result?,
            });
        }
        control.check().map_err(McpRequestError::Control)?;
        let specs = self.host_tools()?;
        let admission = self.admission.as_ref().expect("admitted connection");
        if let ClientRequest::CallToolRequest(call) = request {
            let grant = view
                .grants
                .get(call.params.name.as_ref())
                .ok_or(McpRequestError::Control(OperationError::InventoryChanged))?;
            if grant.policy != self.server.policy
                || grant.policy.exposure_for(call.params.name.as_ref()) == McpExposure::Hidden
            {
                return Err(McpRequestError::Control(OperationError::InventoryChanged));
            }
            // Descriptor membership is additional evidence, not the host's grant. The grant was
            // already enforced by the original ScopedTool/host admission; internal recovery keeps
            // that exact ToolContext/RunId/frozen callable, never a newly looked-up executable.
            if specs.is_some_and(|specs| !specs.contains(&grant.spec)) {
                return Err(McpRequestError::Control(OperationError::InventoryChanged));
            }
            if !Arc::ptr_eq(&self.server.connection, &admission.inventory.connection)
                && !admission
                    .inventory
                    .tools
                    .iter()
                    .any(|remote| remote == &grant.remote)
            {
                return Err(McpRequestError::Control(OperationError::InventoryChanged));
            }
        } else if self.server.policy.exposure == McpExposure::Hidden
            || !admission
                .inventory
                .connection
                .metadata()
                .is_some_and(|info| info.capabilities.resources.is_some())
        {
            return Err(McpRequestError::Control(OperationError::InventoryChanged));
        }
        Ok(admission.inventory.connection.clone())
    }

    /// One SDK request. Aggregate pagination calls this repeatedly under the same operation and
    /// transport admission, and handles recovery OUTSIDE the complete page loop.
    pub(crate) async fn request(
        &mut self,
        request: ClientRequest,
        tool: &str,
        updates: &ToolUpdateSink,
    ) -> Result<ServerResult, McpRequestError> {
        let error = match self.request_once(request, tool, updates).await {
            Ok(result) => return Ok(result),
            Err(error) => error,
        };
        let Some(challenge) = error.http().and_then(auth_challenge) else {
            return Err(error);
        };
        self.control().check().map_err(McpRequestError::Control)?;
        drop(self.host_tools()?);
        let connection = self
            .admission
            .as_ref()
            .map_or(&self.server.connection, |admission| {
                &admission.inventory.connection
            });
        let required = McpAuthorizationRequired {
            server: self.server.name.clone(),
            provider: connection.provider_auth.clone(),
            static_authorization: false,
            challenge: challenge.map(str::to_owned),
        };
        // Terminal auth guidance applies to both APIs, but the needs-auth lifecycle
        // belongs to supervised Coding connections, not standalone explicit pools.
        if let Some(slot) = self
            .server
            .binding
            .as_ref()
            .and_then(ConnectionBinding::supervised_slot)
        {
            slot.require_authorization(connection, required.clone())
                .map_err(McpRequestError::Control)?;
        }
        Err(McpRequestError::AuthorizationRequired {
            required,
            source: Box::new(error),
        })
    }

    async fn request_once(
        &mut self,
        mut request: ClientRequest,
        tool: &str,
        updates: &ToolUpdateSink,
    ) -> Result<ServerResult, McpRequestError> {
        let connection = self.connection(&request).await?;
        let control = self.control();
        let usage = match connection.admit() {
            Ok(usage) => usage,
            Err(_) if self.server.binding.is_none() => {
                connection.handler.stderr.finish().await;
                return Err(McpRequestError::Disconnected(
                    connection.handler.stderr.message("Connection closed"),
                ));
            }
            Err(error) => return Err(McpRequestError::Control(error)),
        };
        if self.server.binding.is_some() {
            RequestControl::new(self.control(), &connection.dispatch).attach(&mut request);
        }
        let mut progress = connection.handler.progress.subscribe();
        let handle = tokio::select! {
            biased;
            () = self.context.signal().wait() => return Err(McpRequestError::Aborted),
            () = control.wait_retired() => return Err(McpRequestError::Control(OperationError::AuthorityRetired)),
            () = tokio::time::sleep_until(self.deadline) => return Err(McpRequestError::Timeout(self.server.policy.timeout)),
            result = connection.peer.send_cancellable_request(request, Default::default()) => match result {
                Ok(handle) => handle,
                Err(error) => return Err(connection.request_error(error).await),
            },
        };
        let mut active = ActiveRequest {
            handle: Some(handle),
            usage: Some(usage),
        };
        loop {
            tokio::select! {
                biased;
                () = self.context.signal().wait() => {
                    active.cancel().await;
                    return Err(McpRequestError::Aborted);
                }
                () = control.wait_retired() => {
                    active.cancel().await;
                    return Err(McpRequestError::Control(OperationError::AuthorityRetired));
                }
                () = tokio::time::sleep_until(self.deadline) => {
                    active.cancel().await;
                    return Err(McpRequestError::Timeout(self.server.policy.timeout));
                }
                result = &mut active.handle.as_mut().expect("active SDK handle").rx => {
                    active.handle.take();
                    // A dropped SDK response sender is itself a closed-transport result.
                    // Do not let `?` bypass stderr settlement and disconnect diagnostics.
                    return match result.unwrap_or(Err(ServiceError::TransportClosed)) {
                        Ok(result) => {
                            self.deadline = tokio::time::Instant::now() + self.server.policy.timeout;
                            Ok(result)
                        }
                        Err(error) => Err(connection.request_error(error).await),
                    };
                }
                notification = progress.recv() => {
                    if let Ok(notification) = notification
                        && notification.progress_token == active.handle.as_ref().expect("active SDK handle").progress_token
                    {
                        self.deadline = tokio::time::Instant::now() + self.server.policy.timeout;
                        let text = notification.message.unwrap_or_else(|| match notification.total {
                            Some(total) => format!("Progress {}/{total}", notification.progress),
                            None => format!("Progress {}", notification.progress),
                        });
                        updates.send(ToolUpdate { content: vec![ContentBlock::Text(TextContent::new(text))], details: Some(serde_json::json!({"server": self.server.name, "tool": tool})) });
                    }
                }
            }
        }
    }

    pub(crate) async fn recover(
        &mut self,
        error: &McpRequestError,
        resource: bool,
    ) -> Result<bool, McpRequestError> {
        let Some(view) = &self.server.binding else {
            return Ok(false);
        };
        let expired = matches!(error.http(), Some(HttpError::SessionExpired));
        // Standalone pools share exact replacement ownership, not Coding's resource
        // retry policy. Session expiry still uses the common one-attempt allowance.
        let transient = resource
            && view.supervised_slot().is_some()
            && matches!(error.http(), Some(HttpError::Client(error))
                if error.status().is_some_and(retry::transient_status));
        if !expired && !transient {
            return Ok(false);
        }
        let control = self.control();
        control.check().map_err(McpRequestError::Control)?;
        if expired && let Some(admission) = &self.admission {
            view.slot()
                .expire(&admission.inventory.connection)
                .map_err(McpRequestError::Control)?;
        }
        if !control.try_recover().map_err(McpRequestError::Control)? {
            return Ok(false);
        }
        self.admission.take();
        if transient {
            tokio::select! {
                biased;
                () = self.context.signal().wait() => return Err(McpRequestError::Aborted),
                () = control.wait_retired() => return Err(McpRequestError::Control(OperationError::AuthorityRetired)),
                () = tokio::time::sleep_until(self.deadline) => return Err(McpRequestError::Timeout(self.server.policy.timeout)),
                () = tokio::time::sleep(Duration::from_millis(250)) => {},
            }
        }
        Ok(true)
    }
}

impl McpConnection {
    async fn request_error(&self, error: ServiceError) -> McpRequestError {
        let error = McpRequestError::from_service(error);
        // The response channel can close before the peer publishes its closed flag.
        // Typed transport closure is sufficient; auth/HTTP/control failures stay typed.
        if matches!(&error, McpRequestError::Service(cause)
            if matches!(cause, ServiceError::TransportClosed)
                || (self.is_closed() && error.http().is_none()))
        {
            self.handler.stderr.finish().await;
            return McpRequestError::Disconnected(self.handler.stderr.message("Connection closed"));
        }
        error
    }
}

impl McpRequestError {
    pub(super) fn from_service(error: ServiceError) -> Self {
        let http = match &error {
            ServiceError::TransportSend(transport) => {
                crate::http_client::transport_error(transport.error.as_ref())
            }
            _ => None,
        };
        // Stream provenance prevents application replay/auth handoff, not cancellation.
        let control_cause = http.map(|error| match error {
            HttpError::Client(client) => client.response_stream_cause().unwrap_or(error),
            _ => error,
        });
        if let Some(HttpError::Io(io)) = control_cause
            && let Some(cause) = io
                .get_ref()
                .and_then(|cause| cause.downcast_ref::<OperationError>())
        {
            return Self::Control(*cause);
        }
        Self::Service(error)
    }

    fn http(&self) -> Option<&HttpError> {
        match self {
            Self::AuthorizationRequired { source, .. } => source.http(),
            Self::Service(ServiceError::TransportSend(error)) => {
                crate::http_client::transport_error(error.error.as_ref())
            }
            _ => None,
        }
    }
}

struct ActiveRequest {
    handle: Option<RequestHandle<RoleClient>>,
    usage: Option<ConnectionUse>,
}

impl ActiveRequest {
    fn cleanup(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        let handle = self.handle.take()?;
        let usage = self.usage.take();
        Some(tokio::spawn(async move {
            let _usage = usage;
            let _ = tokio::time::timeout(
                SHUTDOWN_TIMEOUT,
                handle.cancel(Some("MCP application request cancelled".into())),
            )
            .await;
        }))
    }

    async fn cancel(&mut self) {
        if let Some(task) = self.cleanup() {
            let _ = task.await;
        }
    }
}

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        if self.handle.is_some() && tokio::runtime::Handle::try_current().is_ok() {
            self.cleanup();
        }
    }
}
