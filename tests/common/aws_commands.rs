//! AWS Commands test helper — manages Command resource lifecycle for
//! integration tests.
//!
//! Modeled after [`super::aws_ota`] but slimmer: there is no S3 upload, no
//! code-signing dance, and the trigger API lives in the data plane
//! (`aws-sdk-iotjobsdataplane::start_command_execution`).

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::task::{Context, Poll};

use aws_credential_types::provider::SharedCredentialsProvider;
use aws_sdk_iot::error::ProvideErrorMetadata;
use aws_sdk_iot::primitives::Blob;
use aws_sdk_iot::types::{CommandExecutionStatus, CommandNamespace, CommandPayload};

const TARGET_ACCOUNT_ID: &str = "411974994697";
pub const THING_NAME: &str = "rustot-test";
const REGION: &str = "eu-west-1";
const CROSS_ACCOUNT_ROLE: &str = "ExternalAccessProvisionIoT";

/// Context returned by [`setup`] — holds everything needed for cleanup,
/// triggering, and cloud-side assertions.
pub struct CommandTestContext {
    /// Assumed-role credentials (for IoT operations in the target account)
    iot_creds: SharedCredentialsProvider,
    /// ARN of the Command resource created by setup
    command_arn: String,
    /// ARN of the test thing
    thing_arn: String,
    /// AWS region
    region: aws_config::Region,
    /// Account-specific endpoint for `aws-sdk-iotjobsdataplane`, resolved via
    /// `iot:DescribeEndpoint(endpointType="iot:Jobs")`. The SDK's default
    /// regional endpoint returns AccessDenied for command executions.
    jobs_data_endpoint: String,
}

impl CommandTestContext {
    pub fn command_arn(&self) -> &str {
        &self.command_arn
    }

    /// Trigger a command execution against the test thing and return the
    /// generated `executionId`.
    pub async fn start_execution(&self) -> Result<String, String> {
        let shared = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(self.region.clone())
            .credentials_provider(self.iot_creds.clone())
            .load()
            .await;
        let cfg = aws_sdk_iotjobsdataplane::config::Builder::from(&shared)
            .endpoint_url(&self.jobs_data_endpoint)
            .build();
        let client = aws_sdk_iotjobsdataplane::Client::from_conf(cfg);

        let resp = client
            .start_command_execution()
            .target_arn(&self.thing_arn)
            .command_arn(&self.command_arn)
            .send()
            .await
            .map_err(|e| {
                format!(
                    "StartCommandExecution failed: code={:?} message={:?} raw={:?}",
                    e.code(),
                    e.message(),
                    e
                )
            })?;

        let id = resp
            .execution_id()
            .ok_or("StartCommandExecution returned no executionId")?
            .to_owned();
        log::info!("Started command execution: {id}");
        Ok(id)
    }

    /// Fetch the current cloud-side execution status.
    pub async fn execution_status(
        &self,
        execution_id: &str,
    ) -> Result<CommandExecutionStatus, String> {
        let cfg = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(self.region.clone())
            .credentials_provider(self.iot_creds.clone())
            .load()
            .await;
        let client = aws_sdk_iot::Client::new(&cfg);

        let resp = client
            .get_command_execution()
            .execution_id(execution_id)
            .target_arn(&self.thing_arn)
            .send()
            .await
            .map_err(|e| {
                format!(
                    "GetCommandExecution failed: code={:?} message={:?} raw={:?}",
                    e.code(),
                    e.message(),
                    e
                )
            })?;

        resp.status()
            .cloned()
            .ok_or_else(|| "GetCommandExecution returned no status".to_owned())
    }

    /// Poll [`execution_status`] until a terminal status is observed or the
    /// deadline elapses.
    pub async fn wait_for_terminal(
        &self,
        execution_id: &str,
        deadline: std::time::Duration,
    ) -> Result<CommandExecutionStatus, String> {
        let start = std::time::Instant::now();
        loop {
            let status = self.execution_status(execution_id).await?;
            let terminal = matches!(
                status,
                CommandExecutionStatus::Succeeded
                    | CommandExecutionStatus::Failed
                    | CommandExecutionStatus::Rejected
                    | CommandExecutionStatus::TimedOut
            );
            if terminal {
                return Ok(status);
            }
            if start.elapsed() >= deadline {
                return Err(format!(
                    "Timed out waiting for terminal status (last seen: {status:?})"
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    /// Cleanup: delete the Command resource. Best-effort — failures are logged.
    pub async fn cleanup(&self) {
        log::info!("Cleaning up Command test resources...");

        let cfg = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(self.region.clone())
            .credentials_provider(self.iot_creds.clone())
            .load()
            .await;
        let client = aws_sdk_iot::Client::new(&cfg);

        match client
            .delete_command()
            .command_id(command_id_from_arn(&self.command_arn))
            .send()
            .await
        {
            Ok(_) => log::info!("Deleted Command: {}", self.command_arn),
            Err(e) => log::warn!("Failed to delete Command {}: {}", self.command_arn, e),
        }
    }
}

/// Set up an AWS Command resource for the test.
///
/// Returns `Some(CommandTestContext)` if credentials are available and role
/// assumption succeeds, or `None` if the test should be skipped.
///
/// `payload_bytes` is the raw command payload delivered verbatim to the
/// device on the request topic. `content_type` selects which `request/<fmt>`
/// topic AWS routes to (`application/json` → `request/json`,
/// `application/cbor` → `request/cbor`).
pub async fn setup(payload_bytes: Vec<u8>, content_type: &str) -> Option<CommandTestContext> {
    let region = aws_config::Region::new(REGION);

    let mgmt_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(region.clone())
        .load()
        .await;
    let sts_client = aws_sdk_sts::Client::new(&mgmt_config);

    if let Err(e) = sts_client.get_caller_identity().send().await {
        log::warn!(
            "No valid AWS credentials available, skipping Commands test: {}",
            e
        );
        return None;
    }

    let role_arn = format!("arn:aws:iam::{TARGET_ACCOUNT_ID}:role/{CROSS_ACCOUNT_ROLE}");
    let assumed = match sts_client
        .assume_role()
        .role_arn(&role_arn)
        .role_session_name("RustotCommandsIntegration")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            log::warn!(
                "Failed to assume role {}, skipping Commands test: {}",
                role_arn,
                e
            );
            return None;
        }
    };

    let assumed_creds = assumed.credentials()?;
    let expiration =
        std::time::SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_millis(
            assumed_creds.expiration().to_millis().unwrap_or(0) as u64,
        ));
    let iot_creds_provider = aws_credential_types::Credentials::new(
        assumed_creds.access_key_id(),
        assumed_creds.secret_access_key(),
        Some(assumed_creds.session_token().to_owned()),
        expiration,
        "commands-test-assumed-role",
    );
    let iot_creds = SharedCredentialsProvider::new(iot_creds_provider);

    // Verify identity is in the target account.
    let iot_sts_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(region.clone())
        .credentials_provider(iot_creds.clone())
        .load()
        .await;
    if let Ok(id) = aws_sdk_sts::Client::new(&iot_sts_config)
        .get_caller_identity()
        .send()
        .await
        && id.account().unwrap_or_default() != TARGET_ACCOUNT_ID
    {
        log::warn!(
            "Assumed role is in account {} but expected {}, skipping",
            id.account().unwrap_or("unknown"),
            TARGET_ACCOUNT_ID
        );
        return None;
    }

    // Create the Command resource. The id is unique per run so concurrent
    // tests don't collide.
    let command_id = format!("rustot-test-{}", uuid::Uuid::new_v4());
    let iot_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(region.clone())
        .credentials_provider(iot_creds.clone())
        .load()
        .await;
    let iot_client = aws_sdk_iot::Client::new(&iot_config);

    // Resolve the account-specific Jobs Data endpoint. The default regional
    // endpoint is AccessDenied for command executions.
    let jobs_data_endpoint = match iot_client
        .describe_endpoint()
        .endpoint_type("iot:Jobs")
        .send()
        .await
    {
        Ok(r) => match r.endpoint_address {
            Some(host) => format!("https://{host}"),
            None => {
                log::warn!("DescribeEndpoint(iot:Jobs) returned no address, skipping");
                return None;
            }
        },
        Err(e) => {
            log::warn!("DescribeEndpoint(iot:Jobs) failed, skipping: {e}");
            return None;
        }
    };
    log::info!("Resolved Jobs Data endpoint: {jobs_data_endpoint}");

    let payload = CommandPayload::builder()
        .content(Blob::new(payload_bytes))
        .content_type(content_type.to_owned())
        .build();

    let create_resp = match iot_client
        .create_command()
        .command_id(&command_id)
        .namespace(CommandNamespace::AwsIoT)
        .payload(payload)
        .display_name("Rustot integration test")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            log::warn!("CreateCommand failed, skipping Commands test: {}", e);
            return None;
        }
    };

    let command_arn = create_resp
        .command_arn()
        .expect("CreateCommand returned no ARN")
        .to_owned();

    log::info!("Created Command: {command_arn}");

    let thing_arn = format!("arn:aws:iot:{REGION}:{TARGET_ACCOUNT_ID}:thing/{THING_NAME}");

    Some(CommandTestContext {
        iot_creds,
        command_arn,
        thing_arn,
        region,
        jobs_data_endpoint,
    })
}

/// Async-safe panic catcher (mirrors [`super::aws_ota::catch_unwind_future`]).
pub async fn catch_unwind_future<F, T>(f: F) -> Result<T, Box<dyn std::any::Any + Send>>
where
    F: Future<Output = T>,
{
    CatchUnwindFuture {
        inner: Box::pin(AssertUnwindSafe(f)),
    }
    .await
}

struct CatchUnwindFuture<F> {
    inner: Pin<Box<F>>,
}

impl<F: Future> Future for CatchUnwindFuture<AssertUnwindSafe<F>> {
    type Output = Result<F::Output, Box<dyn std::any::Any + Send>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = &mut self.inner;
        match std::panic::catch_unwind(AssertUnwindSafe(|| inner.as_mut().poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(val)) => Poll::Ready(Ok(val)),
            Err(panic) => Poll::Ready(Err(panic)),
        }
    }
}

fn command_id_from_arn(arn: &str) -> &str {
    arn.rsplit_once('/').map(|(_, id)| id).unwrap_or(arn)
}
