//! Supervised logical operations over frozen callable views and SDK request handles.
use super::*;
use crate::http_client::HttpError;
use crate::supervisor::{ConnectionAdmission, WeakConnectionSlot, WeakMcpGenerationReadiness};
use operation::{LogicalOperation, OperationError, OperationGuard, RequestControl};
use rmcp::service::{RequestHandle, ServiceError};

pub(super) struct CallableGrant {
    remote: McpToolSpec,
    spec: ToolSpec,
    policy: McpServerPolicy,
}

pub(super) struct SupervisedConnection {
    authority: WeakMcpGenerationReadiness,
    slot: WeakConnectionSlot,
    grants: BTreeMap<String, CallableGrant>,
}

impl McpToolSet {
    /// Attach only after the final naming/quarantine pass. Grants are immutable executable-view
    /// data; raw supervisor slots never own or infer policy from names.
    pub(crate) fn with_supervision(
        &self,
        authority: WeakMcpGenerationReadiness,
    ) -> Result<Self, OperationError> {
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
                supervision: Some(SupervisedConnection {
                    slot: authority
                        .connection_slot(&old.name)
                        .ok_or(OperationError::AuthorityRetired)?,
                    authority: authority.clone(),
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
                self.supervision.as_ref().map(|view| view.authority.clone()),
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
        self.context
            .session
            .tools()
            .map(Some)
            .map_err(|_| McpRequestError::Control(OperationError::AuthorityRetired))
    }

    async fn connection(
        &mut self,
        request: &ClientRequest,
    ) -> Result<Arc<McpConnection>, McpRequestError> {
        let control = self.control();
        control.check().map_err(McpRequestError::Control)?;
        let Some(view) = &self.server.supervision else {
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
                result = view.slot.acquire() => result.map_err(McpRequestError::Control)?,
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
        mut request: ClientRequest,
        tool: &str,
        updates: &ToolUpdateSink,
    ) -> Result<ServerResult, McpRequestError> {
        let connection = self.connection(&request).await?;
        let control = self.control();
        let usage = match connection.admit() {
            Ok(usage) => usage,
            Err(_) if self.server.supervision.is_none() => {
                connection.handler.stderr.finish().await;
                return Err(McpRequestError::Disconnected(
                    connection.handler.stderr.message("Connection closed"),
                ));
            }
            Err(error) => return Err(McpRequestError::Control(error)),
        };
        if self.server.supervision.is_some() {
            RequestControl::new(self.control(), &connection.dispatch).attach(&mut request);
        }
        let mut progress = connection.handler.progress.subscribe();
        let handle = tokio::select! {
            biased;
            () = self.context.signal().wait() => return Err(McpRequestError::Aborted),
            () = control.wait_retired() => return Err(McpRequestError::Control(OperationError::AuthorityRetired)),
            () = tokio::time::sleep_until(self.deadline) => return Err(McpRequestError::Timeout(self.server.policy.timeout)),
            result = connection.peer.send_cancellable_request(request, Default::default()) => result.map_err(McpRequestError::from_service)?,
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
                    let result = result.map_err(|_| McpRequestError::Service(ServiceError::TransportClosed))?
                        .map_err(McpRequestError::from_service);
                    if result.is_ok() { self.deadline = tokio::time::Instant::now() + self.server.policy.timeout; }
                    if matches!(result, Err(McpRequestError::Service(_))) && connection.is_closed() {
                        connection.handler.stderr.finish().await;
                        return Err(McpRequestError::Disconnected(connection.handler.stderr.message("Connection closed")));
                    }
                    return result;
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
        let Some(view) = &self.server.supervision else {
            return Ok(false);
        };
        let expired = matches!(error.http(), Some(HttpError::SessionExpired));
        let transient = resource
            && matches!(error.http(), Some(HttpError::Client(error))
                if error.status().is_some_and(retry::transient_status));
        if !expired && !transient {
            return Ok(false);
        }
        let control = self.control();
        control.check().map_err(McpRequestError::Control)?;
        if expired && let Some(admission) = &self.admission {
            view.slot
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

impl McpRequestError {
    pub(super) fn from_service(error: ServiceError) -> Self {
        if let ServiceError::TransportSend(transport) = &error
            && let Some(HttpError::Io(io)) = transport.error.downcast_ref::<HttpError>()
            && let Some(cause) = io
                .get_ref()
                .and_then(|cause| cause.downcast_ref::<OperationError>())
        {
            return Self::Control(*cause);
        }
        Self::Service(error)
    }

    fn http(&self) -> Option<&HttpError> {
        let Self::Service(ServiceError::TransportSend(error)) = self else {
            return None;
        };
        error.error.downcast_ref()
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
