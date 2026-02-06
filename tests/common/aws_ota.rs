//! AWS OTA test helper — manages OTA job lifecycle for integration tests.
//!
//! Handles credential loading, cross-account role assumption, S3 upload,
//! OTA job creation/cleanup, and cloud-side job status assertions.

use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::task::{Context, Poll};

use aws_credential_types::provider::SharedCredentialsProvider;
use aws_sdk_iot::types::JobExecutionStatus;

const TARGET_ACCOUNT_ID: &str = "411974994697";
const THING_NAME: &str = "rustot-test";
const REGION: &str = "eu-west-1";
const CROSS_ACCOUNT_ROLE: &str = "ExternalAccessProvisionIoT";
const OTA_UPDATE_ROLE: &str = "IoTOTAUpdateRole";

/// Context returned by [`setup`] — holds everything needed for cleanup and assertions.
pub struct OtaTestContext {
    /// MGMT credentials (for S3 operations)
    mgmt_creds: SharedCredentialsProvider,
    /// Assumed-role credentials (for IoT operations in the target account)
    iot_creds: SharedCredentialsProvider,
    /// The AWS IoT Job ID created by the OTA update
    job_id: String,
    /// S3 bucket used for the OTA file
    s3_bucket: String,
    /// S3 key used for the OTA file
    s3_key: String,
    /// The AWS region
    region: aws_config::Region,
}

impl OtaTestContext {
    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    pub fn iot_creds(&self) -> SharedCredentialsProvider {
        self.iot_creds.clone()
    }

    pub fn region(&self) -> &aws_config::Region {
        &self.region
    }

    /// Describe the job execution for the OTA update on our thing.
    ///
    /// Returns `(JobExecutionStatus, HashMap<String, String>)` with the status
    /// and any status details from the job execution.
    pub async fn describe_job_execution(
        &self,
    ) -> Result<(JobExecutionStatus, HashMap<String, String>), String> {
        let iot_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(self.region.clone())
            .credentials_provider(self.iot_creds.clone())
            .load()
            .await;
        let iot_client = aws_sdk_iot::Client::new(&iot_config);

        log::info!("Describing job execution for job ID: {}", self.job_id);

        let execution = iot_client
            .describe_job_execution()
            .job_id(&self.job_id)
            .thing_name(THING_NAME)
            .send()
            .await
            .map_err(|e| format!("DescribeJobExecution failed: {e}"))?;

        let exec = execution.execution().ok_or("No job execution found")?;

        let status = exec.status().ok_or("No status in job execution")?.clone();

        let details: HashMap<String, String> = exec
            .status_details()
            .and_then(|d| d.details_map())
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();

        log::info!("Job execution status: {:?}, details: {:?}", status, details);

        Ok((status, details))
    }

    /// Cleanup: cancel any non-terminal jobs and delete the S3 object.
    pub async fn cleanup(&self) {
        log::info!("Cleaning up OTA test resources...");

        // Cancel any stale jobs
        if let Err(e) = cancel_stale_jobs(&self.iot_creds, &self.region).await {
            log::warn!("Failed to cancel stale jobs during cleanup: {}", e);
        }

        // Delete S3 object
        let s3_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(self.region.clone())
            .credentials_provider(self.mgmt_creds.clone())
            .load()
            .await;
        let s3_client = aws_sdk_s3::Client::new(&s3_config);

        match s3_client
            .delete_object()
            .bucket(&self.s3_bucket)
            .key(&self.s3_key)
            .send()
            .await
        {
            Ok(_) => log::info!("Deleted S3 object s3://{}/{}", self.s3_bucket, self.s3_key),
            Err(e) => log::warn!("Failed to delete S3 object: {}", e),
        }
    }

    /// Assert that the cloud-side job execution has status `SUCCEEDED`.
    #[allow(dead_code)]
    pub async fn assert_job_succeeded(&self) {
        let (status, details) = self
            .describe_job_execution()
            .await
            .expect("Failed to describe job execution");
        assert_eq!(
            status,
            JobExecutionStatus::Succeeded,
            "Expected job status SUCCEEDED, got {:?} with details: {:?}",
            status,
            details
        );
    }

    /// Assert that the cloud-side job execution has status `FAILED`.
    #[allow(dead_code)]
    pub async fn assert_job_failed(&self) {
        let (status, details) = self
            .describe_job_execution()
            .await
            .expect("Failed to describe job execution");
        assert_eq!(
            status,
            JobExecutionStatus::Failed,
            "Expected job status FAILED, got {:?} with details: {:?}",
            status,
            details
        );
    }
}

/// Set up AWS OTA test resources.
///
/// Returns `Some(OtaTestContext)` if credentials are available and role
/// assumption succeeds, or `None` if the test should be skipped.
pub async fn setup() -> Option<OtaTestContext> {
    let region = aws_config::Region::new(REGION);

    // Load default MGMT credentials from environment
    let mgmt_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(region.clone())
        .load()
        .await;

    let sts_client = aws_sdk_sts::Client::new(&mgmt_config);

    // Verify MGMT credentials work
    let caller = match sts_client.get_caller_identity().send().await {
        Ok(c) => c,
        Err(e) => {
            log::warn!(
                "No valid AWS credentials available, skipping OTA test: {}",
                e
            );
            return None;
        }
    };

    let mgmt_account = caller.account().unwrap_or("unknown");
    log::info!("MGMT account: {}", mgmt_account);

    // Store MGMT credentials provider
    let mgmt_creds = mgmt_config
        .credentials_provider()
        .expect("MGMT config must have credentials")
        .clone();

    // Assume role into the target IoT account
    let role_arn = format!("arn:aws:iam::{TARGET_ACCOUNT_ID}:role/{CROSS_ACCOUNT_ROLE}");
    let assume_result = match sts_client
        .assume_role()
        .role_arn(&role_arn)
        .role_session_name("RustotIntegration")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            log::warn!(
                "Failed to assume role {}, skipping OTA test: {}",
                role_arn,
                e
            );
            return None;
        }
    };

    let assumed_creds = assume_result.credentials()?;
    let expiration =
        std::time::SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_millis(
            assumed_creds.expiration().to_millis().unwrap_or(0) as u64,
        ));
    let iot_creds_provider = aws_credential_types::Credentials::new(
        assumed_creds.access_key_id(),
        assumed_creds.secret_access_key(),
        Some(assumed_creds.session_token().to_owned()),
        expiration,
        "ota-test-assumed-role",
    );
    let iot_creds = SharedCredentialsProvider::new(iot_creds_provider);

    // Verify assumed identity is in the target account
    let iot_sts_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(region.clone())
        .credentials_provider(iot_creds.clone())
        .load()
        .await;
    let iot_sts_client = aws_sdk_sts::Client::new(&iot_sts_config);

    match iot_sts_client.get_caller_identity().send().await {
        Ok(id) => {
            let account = id.account().unwrap_or("unknown");
            if account != TARGET_ACCOUNT_ID {
                log::warn!(
                    "Assumed role is in account {} but expected {}, skipping",
                    account,
                    TARGET_ACCOUNT_ID
                );
                return None;
            }
            log::info!("Assumed role identity: {}", id.arn().unwrap_or("unknown"));
        }
        Err(e) => {
            log::warn!("Failed to verify assumed role identity: {}", e);
            return None;
        }
    }

    // Pre-cleanup: cancel stale jobs
    if let Err(e) = cancel_stale_jobs(&iot_creds, &region).await {
        log::warn!("Pre-cleanup failed (continuing anyway): {}", e);
    }

    // Upload OTA file to S3 with MGMT creds
    let s3_bucket = format!("{}-{}-build-artifacts", mgmt_account, REGION);
    let update_id = uuid::Uuid::new_v4().to_string();
    let s3_key = format!("factbird-duo/test/rustot/ota/ota_file-{}", update_id);

    let s3_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(region.clone())
        .credentials_provider(mgmt_creds.clone())
        .load()
        .await;
    let s3_client = aws_sdk_s3::Client::new(&s3_config);

    let ota_file_data =
        std::fs::read("tests/assets/ota_file").expect("Failed to read tests/assets/ota_file");

    s3_client
        .put_object()
        .bucket(&s3_bucket)
        .key(&s3_key)
        .body(ota_file_data.into())
        .send()
        .await
        .expect("Failed to upload OTA file to S3");

    log::info!("Uploaded OTA file to s3://{}/{}", s3_bucket, s3_key);

    // Create OTA update with assumed-role creds
    let iot_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(region.clone())
        .credentials_provider(iot_creds.clone())
        .load()
        .await;
    let iot_client = aws_sdk_iot::Client::new(&iot_config);

    let signature = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        b"This is my custom signature\n",
    );

    let ota_update_role_arn = format!("arn:aws:iam::{TARGET_ACCOUNT_ID}:role/{OTA_UPDATE_ROLE}");
    let thing_arn = format!("arn:aws:iot:{REGION}:{TARGET_ACCOUNT_ID}:thing/{THING_NAME}");

    let file_location = aws_sdk_iot::types::FileLocation::builder()
        .s3_location(
            aws_sdk_iot::types::S3Location::builder()
                .bucket(&s3_bucket)
                .key(&s3_key)
                .build(),
        )
        .build();

    let code_signing = aws_sdk_iot::types::CodeSigning::builder()
        .custom_code_signing(
            aws_sdk_iot::types::CustomCodeSigning::builder()
                .signature(
                    aws_sdk_iot::types::CodeSigningSignature::builder()
                        .inline_document(aws_sdk_iot::primitives::Blob::new(
                            base64::Engine::decode(
                                &base64::engine::general_purpose::STANDARD,
                                &signature,
                            )
                            .expect("Failed to decode signature"),
                        ))
                        .build(),
                )
                .certificate_chain(
                    aws_sdk_iot::types::CodeSigningCertificateChain::builder()
                        .certificate_name("cert")
                        .inline_document("signCert")
                        .build(),
                )
                .hash_algorithm("sha256")
                .signature_algorithm("ecdsa")
                .build(),
        )
        .build();

    let ota_file = aws_sdk_iot::types::OtaUpdateFile::builder()
        .file_name("ota_file_OTA")
        .file_location(file_location)
        .code_signing(code_signing)
        .set_file_type(Some(0))
        .build();

    let create_result = iot_client
        .create_ota_update()
        .ota_update_id(&update_id)
        .description("RustOT OTA integration test")
        .targets(&thing_arn)
        .protocols(aws_sdk_iot::types::Protocol::Mqtt)
        .target_selection(aws_sdk_iot::types::TargetSelection::Snapshot)
        .role_arn(&ota_update_role_arn)
        .files(ota_file)
        .send()
        .await
        .expect("Failed to create OTA update");

    // The IoT Job ID follows the convention AFR_OTA-{ota_update_id}.
    // It may not be in the CreateOtaUpdate response yet (CreatePending).
    let job_id = format!("AFR_OTA-{update_id}");

    log::info!(
        "Created OTA update: {} (status: {:?}, job_id: {})",
        update_id,
        create_result.ota_update_status(),
        job_id,
    );

    // Poll until the IoT Job execution exists for our thing (the OTA update
    // starts as CreatePending and the job execution may not exist immediately)
    for attempt in 1..=15 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        match iot_client
            .describe_job_execution()
            .job_id(&job_id)
            .thing_name(THING_NAME)
            .send()
            .await
        {
            Ok(resp) => {
                if let Some(exec) = resp.execution() {
                    log::info!(
                        "Job execution ready: {:?} (attempt {}/15)",
                        exec.status(),
                        attempt
                    );
                    break;
                }
            }
            Err(e) => {
                log::info!(
                    "Job execution not ready yet (attempt {}/15): {}",
                    attempt,
                    e
                );
            }
        }
        if attempt == 15 {
            panic!(
                "Job execution for {} on {} did not appear after 30s",
                job_id, THING_NAME
            );
        }
    }

    Some(OtaTestContext {
        mgmt_creds,
        iot_creds,
        job_id,
        s3_bucket,
        s3_key,
        region,
    })
}

/// Cancel all QUEUED and IN_PROGRESS job executions for our thing,
/// then wait until none remain (so the next test won't pick them up).
async fn cancel_stale_jobs(
    iot_creds: &SharedCredentialsProvider,
    region: &aws_config::Region,
) -> Result<(), String> {
    let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(region.clone())
        .credentials_provider(iot_creds.clone())
        .load()
        .await;
    let client = aws_sdk_iot::Client::new(&config);

    let mut canceled_any = false;

    for status in [
        aws_sdk_iot::types::JobExecutionStatus::Queued,
        aws_sdk_iot::types::JobExecutionStatus::InProgress,
    ] {
        let executions = client
            .list_job_executions_for_thing()
            .thing_name(THING_NAME)
            .status(status.clone())
            .send()
            .await
            .map_err(|e| format!("ListJobExecutionsForThing({:?}) failed: {}", status, e))?;

        for summary in executions.execution_summaries() {
            if let Some(job_id) = summary.job_id() {
                log::info!("Canceling stale job: {} (status: {:?})", job_id, status);
                match client.cancel_job().job_id(job_id).force(true).send().await {
                    Ok(_) => {
                        log::info!("Canceled job: {}", job_id);
                        canceled_any = true;
                    }
                    Err(e) => log::warn!("Failed to cancel job {}: {}", job_id, e),
                }
            }
        }
    }

    // If we canceled anything, wait until no QUEUED/IN_PROGRESS jobs remain
    // so the next test's device won't pick up a stale job via $next
    if canceled_any {
        for attempt in 1..=10 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;

            let mut stale_count = 0;
            for status in [
                aws_sdk_iot::types::JobExecutionStatus::Queued,
                aws_sdk_iot::types::JobExecutionStatus::InProgress,
            ] {
                if let Ok(resp) = client
                    .list_job_executions_for_thing()
                    .thing_name(THING_NAME)
                    .status(status)
                    .send()
                    .await
                {
                    stale_count += resp.execution_summaries().len();
                }
            }

            if stale_count == 0 {
                log::info!("All stale jobs cleared (attempt {}/10)", attempt);
                break;
            }
            log::info!(
                "Still {} stale job(s) remaining (attempt {}/10)",
                stale_count,
                attempt
            );
        }
    }

    Ok(())
}

/// Force-cancel an AWS IoT OTA job.
///
/// A force-cancel immediately transitions IN_PROGRESS executions to `CANCELED`
/// and triggers a `notify-next` notification to the device. The device detects
/// this via its subscription to the notify-next topic and aborts the download.
pub async fn force_cancel_job(
    job_id: &str,
    iot_creds: &SharedCredentialsProvider,
    region: &aws_config::Region,
) -> Result<(), String> {
    let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(region.clone())
        .credentials_provider(iot_creds.clone())
        .load()
        .await;
    let client = aws_sdk_iot::Client::new(&config);

    client
        .cancel_job()
        .job_id(job_id)
        .force(true)
        .send()
        .await
        .map_err(|e| format!("Force-cancel job {} failed: {}", job_id, e))?;
    log::info!("Force-cancelled job: {}", job_id);

    Ok(())
}

/// Async-safe panic catcher.
///
/// Wraps a future so that panics during polling are caught and returned as
/// `Err(Box<dyn Any>)` instead of unwinding the test harness. This ensures
/// cleanup code always runs even if the test panics.
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
