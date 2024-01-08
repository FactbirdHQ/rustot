//! ## Over-the-air (OTA) flashing of firmware
//!
//! AWS IoT OTA works by using AWS IoT Jobs to manage firmware transfer and
//! status reporting of OTA.
//!
//! The OTA Jobs API makes use of the following special MQTT Topics:
//! - $aws/things/{thing_name}/jobs/$next/get/accepted
//! - $aws/things/{thing_name}/jobs/notify-next
//! - $aws/things/{thing_name}/jobs/$next/get
//! - $aws/things/{thing_name}/jobs/{job_id}/update
//! - $aws/things/{thing_name}/streams/{stream_id}/data/cbor
//! - $aws/things/{thing_name}/streams/{stream_id}/get/cbor
//!
//! Most of the data structures for the Jobs API has been copied from Rusoto:
//! <https://docs.rs/rusoto_iot_jobs_data/0.43.0/rusoto_iot_jobs_data/>
//!
//! ### OTA Flow:
//! 1. Device subscribes to notification topics for AWS IoT jobs and listens for
//!    update messages.
//! 2. When an update is available, the OTA agent publishes requests to AWS IoT
//!    and receives updates using the HTTP or MQTT protocol, depending on the
//!    settings you chose.
//! 3. The OTA agent checks the digital signature of the downloaded files and,
//!    if the files are valid, installs the firmware update to the appropriate
//!    flash bank.
//!
//! The OTA depends on working, and correctly setup:
//! - Bootloader
//! - MQTT Client
//! - Code sign verification
//! - CBOR deserializer

pub mod config;
pub mod control_interface;
pub mod data_interface;
pub mod encoding;
pub mod error;
pub mod pal;

#[cfg(feature = "ota_mqtt_data")]
pub use data_interface::mqtt::{Encoding, Topic};

use crate::{jobs::data_types::JobStatus, ota::encoding::json::JobStatusReason};

use self::{
    control_interface::ControlInterface,
    data_interface::DataInterface,
    encoding::FileContext,
    pal::{ImageState, ImageStateReason},
};

#[derive(PartialEq)]
pub struct JobEventData<'a> {
    pub job_name: &'a str,
    pub ota_document: &'a encoding::json::OtaJob<'a>,
    pub status_details: Option<&'a crate::jobs::StatusDetails<'a>>,
}

pub struct Updater;

impl Updater {
    pub async fn perform_ota<'a, C: ControlInterface, D: DataInterface>(
        control: &C,
        data: &D,
        job_data: JobEventData<'a>,
        pal: &mut impl pal::OtaPal,
        config: config::Config,
    ) -> Result<(), error::OtaError> {
        let mut request_momentum = 0;

        // TODO: Handle request_momentum?
        control.request_job().await?;

        let JobEventData {
            job_name,
            ota_document,
            status_details,
        } = job_data;

        let file_idx = 0;

        if ota_document
            .files
            .get(file_idx)
            .map(|f| f.filesize)
            .unwrap_or_default()
            == 0
        {
            return Err(error::OtaError::ZeroFileSize);
        }

        let mut file_ctx = FileContext::new_from(
            job_name,
            ota_document,
            status_details.map(|s| {
                s.iter()
                    .map(|(&k, &v)| {
                        (
                            heapless::String::try_from(k).unwrap(),
                            heapless::String::try_from(v).unwrap(),
                        )
                    })
                    .collect()
            }),
            file_idx,
            &config,
        )?;

        // If the job is in self test mode, don't start an OTA update but
        // instead do the following:
        //
        // If the firmware that performed the update was older than the
        // currently running firmware, set the image state to "Testing." This is
        // the success path.
        //
        // If it's the same or newer, reject the job since either the firmware
        // was not accepted during self test or an incorrect image was sent by
        // the OTA operator.
        let platform_self_test = pal
            .get_platform_image_state()
            .await
            .map_or(false, |i| i == pal::PalImageState::PendingCommit);

        match (file_ctx.self_test(), platform_self_test) {
            (true, true) => {
                // Run self-test!
                Self::set_image_state_with_reason(
                    control,
                    pal,
                    &config,
                    &mut file_ctx,
                    ImageState::Testing(ImageStateReason::VersionCheck),
                )
                .await?;

                info!("Beginning self-test");

                let test_fut = pal.complete_callback(pal::OtaEvent::StartTest);

                match config.self_test_timeout {
                    Some(timeout) => embassy_time::with_timeout(timeout, test_fut)
                        .await
                        .map_err(|_| error::OtaError::Timeout)?,
                    None => test_fut.await,
                }?;

                control
                    .update_job_status(
                        &mut file_ctx,
                        &config,
                        JobStatus::Succeeded,
                        JobStatusReason::Accepted,
                    )
                    .await?;

                return Ok(());
            }
            (false, false) => {}
            (false, true) => {
                // Received a job that is not in self-test but platform is, so
                // reboot the device to allow roll back to previous image.
                error!("Rejecting new image and rebooting: The platform is in the self-test state while the job is not.");
                pal.reset_device().await?;
            }
            (true, false) => {
                // The job is in self test but the platform image state is not so it
                // could be an attack on the platform image state. Reject the update
                // (this should also cause the image to be erased), aborting the job
                // and reset the device.
                error!("Rejecting new image and rebooting: the job is in the self-test state while the platform is not.");
                Self::set_image_state_with_reason(
                    control,
                    pal,
                    &config,
                    &mut file_ctx,
                    ImageState::Rejected(ImageStateReason::ImageStateMismatch),
                )
                .await?;
                pal.reset_device().await?;
            }
        }

        if !ota_document.protocols.contains(&D::PROTOCOL) {
            error!("Unable to handle current OTA job with given data interface ({:?}). Supported protocols: {:?}. Aborting current update.", D::PROTOCOL, ota_document.protocols);
            Self::set_image_state_with_reason(
                control,
                pal,
                &config,
                &mut file_ctx,
                ImageState::Aborted(ImageStateReason::InvalidDataProtocol),
            )
            .await?;
            return Err(error::OtaError::InvalidInterface);
        }

        info!("Job document was accepted. Attempting to begin the update");

        // Create/Open the OTA file on the file system
        if let Err(e) = pal.create_file_for_rx(&file_ctx).await {
            Self::set_image_state_with_reason(
                control,
                pal,
                &config,
                &mut file_ctx,
                ImageState::Aborted(ImageStateReason::Pal(e)),
            )
            .await?;

            pal.close_file(&file_ctx).await?;
            return Err(e.into());
        }

        // Prepare the storage layer on receiving a new file
        match data.init_file_transfer(&mut file_ctx).await {
            Err(e) => {
                return if request_momentum < config.max_request_momentum {
                    // Start request timer
                    // self.request_timer
                    //     .start(config.request_wait.millis())
                    //     .map_err(|_| error::OtaError::Timer)?;

                    request_momentum += 1;
                    Err(e)
                } else {
                    // Stop request timer
                    // self.request_timer
                    //     .cancel()
                    //     .map_err(|_| error::OtaError::Timer)?;

                    // Too many requests have been sent without a response or
                    // too many failures when trying to publish the request
                    // message. Abort.

                    Err(error::OtaError::MomentumAbort)
                };
            }
            Ok(_) => {
                // Reset the request momentum
                request_momentum = 0;

                // TODO: Reset the OTA statistics

                info!("Initialized file handler! Requesting file blocks");
            }
        }

        // Request data
        if file_ctx.blocks_remaining > 0 {
            if request_momentum <= config.max_request_momentum {
                // Each request increases the momentum until a response is
                // received. Too much momentum is interpreted as a failure to
                // communicate and will cause us to abort the OTA.
                request_momentum += 1;

                // Request data blocks
                data.request_file_block(&mut file_ctx, &config).await?;
            } else {
                // Stop the request timer
                // self.request_timer.cancel().map_err(|_| error::OtaError::Timer)?;

                // Failed to send data request abort and close file.
                Self::set_image_state_with_reason(
                    control,
                    pal,
                    &config,
                    &mut file_ctx,
                    ImageState::Aborted(ImageStateReason::MomentumAbort),
                )
                .await?;

                // Reset the request momentum
                request_momentum = 0;

                // Too many requests have been sent without a response or too
                // many failures when trying to publish the request message.
                // Abort.
                return Err(error::OtaError::MomentumAbort);
            }
        } else {
            return Err(error::OtaError::BlockOutOfRange);
        }

        Ok(())
    }

    async fn set_image_state_with_reason<'a, C: ControlInterface, PAL: pal::OtaPal>(
        control: &C,
        pal: &mut PAL,
        config: &config::Config,
        file_ctx: &mut FileContext,
        image_state: ImageState,
    ) -> Result<(), error::OtaError> {
        // debug!("set_image_state_with_reason {:?}", image_state);
        // Call the platform specific code to set the image state

        // FIXME:
        let image_state = match pal.set_platform_image_state(image_state).await {
            Err(e) if !matches!(image_state, ImageState::Aborted(_)) => {
                // If the platform image state couldn't be set correctly, force
                // fail the update by setting the image state to "Rejected"
                // unless it's already in "Aborted".

                // Capture the failure reason if not already set (and we're not
                // already Aborted as checked above). Otherwise Keep the
                // original reject reason code since it is possible for the PAL
                // to fail to update the image state in some cases (e.g. a reset
                // already caused the bundle rollback and we failed to rollback
                // again).

                // Intentionally override reason since we failed within this
                // function
                ImageState::Rejected(ImageStateReason::Pal(e))
            }
            _ => image_state,
        };

        // Now update the image state and job status on server side
        match image_state {
            ImageState::Testing(_) => {
                // We discovered we're ready for test mode, put job status
                // in self_test active
                control
                    .update_job_status(
                        file_ctx,
                        config,
                        JobStatus::InProgress,
                        JobStatusReason::SelfTestActive,
                    )
                    .await?;
            }
            ImageState::Accepted => {
                // Now that we have accepted the firmware update, we can
                // complete the job
                control
                    .update_job_status(
                        file_ctx,
                        config,
                        JobStatus::Succeeded,
                        JobStatusReason::Accepted,
                    )
                    .await?;
            }
            ImageState::Rejected(_) => {
                // The firmware update was rejected, complete the job as
                // FAILED (Job service will not allow us to set REJECTED
                // after the job has been started already).
                control
                    .update_job_status(
                        file_ctx,
                        config,
                        JobStatus::Failed,
                        JobStatusReason::Rejected,
                    )
                    .await?;
            }
            _ => {
                // The firmware update was aborted, complete the job as
                // FAILED (Job service will not allow us to set REJECTED
                // after the job has been started already).
                control
                    .update_job_status(
                        file_ctx,
                        config,
                        JobStatus::Failed,
                        JobStatusReason::Aborted,
                    )
                    .await?;
            }
        }
        Ok(())
    }
}
