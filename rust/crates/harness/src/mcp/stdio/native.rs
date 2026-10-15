use crate::{Error, OperationId, Result, mcp::McpToolTransport, runtime::ToolContext};
use acyclic_native_runtime::{ProcessTree, run_blocking_io};
use acyclic_stream::BoxProviderFuture;
use futures::StreamExt as _;
use rmcp::{
    RoleClient, ServiceExt as _,
    model::{
        ClientConfig, ClientJsonRpcMessage, ClientRequest, CustomRequest, Implementation,
        ProtocolVersion, ServerJsonRpcMessage,
    },
    service::RunningService,
};
use serde_json::{Value, json};
use std::{
    process::{Command, Stdio},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{FramedRead, FramedWrite};

mod codec;
use codec::BoundedCodec;

/// One initialized, bounded MCP connection and its owned native process tree.
pub struct StdioMcpTransport {
    service: RunningService<RoleClient, ClientConfig>,
    timeout: Duration,
    _process: Option<std::sync::Mutex<ProcessTree>>,
}

impl StdioMcpTransport {
    /// Starts one host-selected executable and keeps it for the connection's lifetime.
    /// Arguments, environment and workspace are supplied through `command`.
    pub async fn spawn(mut command: Command, maximum_bytes: u32, timeout_ms: u32) -> Result<Self> {
        validate_bounds(maximum_bytes, timeout_ms)?;
        acyclic_native_runtime::initialize_native_io_services().map_err(Error::process_io)?;
        let timeout = Duration::from_millis(u64::from(timeout_ms));
        tokio::time::timeout(timeout, async move {
            let mut process = run_blocking_io(
                move || {
                    command
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::inherit());
                    ProcessTree::spawn(&mut command)
                },
                None,
            )
            .map_err(Error::process_io)?
            .await
            .map_err(Error::process_io)?
            .map_err(Error::process_io)?;
            let input = process
                .take_stdin()
                .ok_or_else(|| Error::Invalid("MCP stdin pipe missing".into()))?;
            let output = process
                .take_stdout()
                .ok_or_else(|| Error::Invalid("MCP stdout pipe missing".into()))?;
            let input = tokio::process::ChildStdin::from_std(input).map_err(Error::process_io)?;
            let output =
                tokio::process::ChildStdout::from_std(output).map_err(Error::process_io)?;
            Self::bind(output, input, maximum_bytes, timeout, Some(process)).await
        })
        .await
        .map_err(|_| Error::Process {
            kind: "timeout",
            message: "MCP initialization deadline elapsed".into(),
        })?
    }

    async fn bind<R, W>(
        read: R,
        write: W,
        maximum_bytes: u32,
        timeout: Duration,
        process: Option<ProcessTree>,
    ) -> Result<Self>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let read = FramedRead::new(
            read,
            BoundedCodec::<ServerJsonRpcMessage>::new(maximum_bytes as usize),
        )
        .take_while(|message| std::future::ready(message.is_ok()))
        .filter_map(|message| std::future::ready(message.ok()));
        let write = FramedWrite::new(
            write,
            BoundedCodec::<ClientJsonRpcMessage>::new(maximum_bytes as usize),
        );
        let client = ClientConfig::new(
            Default::default(),
            Implementation::new("acyclic-harness", env!("CARGO_PKG_VERSION")),
        )
        .with_protocol_version(ProtocolVersion::V_2025_11_25);
        let service = client
            .serve((write, read))
            .await
            .map_err(|error| Error::Invalid(format!("MCP initialization: {error}")))?;
        if service
            .peer_info()
            .is_none_or(|info| info.protocol_version != ProtocolVersion::V_2025_11_25)
        {
            return Err(Error::Unsupported(
                "MCP protocol version differs from the selected binding".into(),
            ));
        }
        Ok(Self {
            service,
            timeout,
            _process: process.map(std::sync::Mutex::new),
        })
    }

    /// Exchanges a supported service request without reconnecting or retrying it.
    pub async fn rpc(
        &self,
        operation: OperationId,
        method: &str,
        params: Value,
        context: Option<ToolContext>,
    ) -> Result<Value> {
        crate::mcp::validate_operation_context(operation, context.as_ref())?;
        super::services::validate_params(method, &params, None)?;
        let request = ClientRequest::CustomRequest(CustomRequest::new(method, Some(params)));
        let result = tokio::time::timeout(self.timeout, self.service.send_request(request))
            .await
            .map_err(|_| Error::Indeterminate(operation))?
            .map_err(|_| Error::Indeterminate(operation))?;
        let result =
            serde_json::to_value(result).map_err(|error| Error::Invalid(error.to_string()))?;
        super::services::validate_result(method, result, None)
    }
}

impl McpToolTransport for StdioMcpTransport {
    fn call<'a>(
        &'a self,
        operation: OperationId,
        name: &'a str,
        arguments: Value,
        context: Option<ToolContext>,
    ) -> BoxProviderFuture<'a, Result<Value>> {
        Box::pin(async move {
            self.rpc(
                operation,
                "tools/call",
                json!({"name":name, "arguments":arguments}),
                context,
            )
            .await
        })
    }

    fn reconcile<'a>(
        &'a self,
        operation: OperationId,
        context: Option<ToolContext>,
    ) -> BoxProviderFuture<'a, Result<Option<Value>>> {
        Box::pin(async move {
            crate::mcp::validate_operation_context(operation, context.as_ref())?;
            // A protocol request ID is not a durable server receipt.
            Ok(None)
        })
    }
}

fn validate_bounds(maximum_bytes: u32, timeout_ms: u32) -> Result<()> {
    if maximum_bytes == 0
        || timeout_ms == 0
        || usize::try_from(maximum_bytes)
            .ok()
            .and_then(|bytes| bytes.checked_add(1))
            .is_none()
    {
        return Err(Error::Invalid(
            "MCP stdio needs positive finite bounds".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
