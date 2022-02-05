use super::{
    config::Config,
    data_interface::Protocol,
    encoding::{
        json::{FileDescription, OtaJob},
        FileContext,
    },
    pal::Version,
};

pub mod mock;

pub fn test_job_doc() -> OtaJob<'static> {
    OtaJob {
        protocols: heapless::Vec::from_slice(&[Protocol::Mqtt]).unwrap(),
        streamname: "test_stream",
        files: heapless::Vec::from_slice(&[FileDescription {
            filepath: "",
            filesize: 123456,
            fileid: 0,
            certfile: "cert",
            update_data_url: None,
            auth_scheme: None,
            sha1_rsa: Some(heapless::String::from("")),
            file_type: Some(0),
            sha256_rsa: None,
            sha1_ecdsa: None,
            sha256_ecdsa: None,
        }])
        .unwrap(),
    }
}

pub fn test_file_ctx(config: &Config) -> FileContext {
    let ota_job = test_job_doc();
    FileContext::new_from("Job-name", &ota_job, None, 0, config, Version::default()).unwrap()
}

pub mod ota_tests {
    use crate::jobs::data_types::{DescribeJobExecutionResponse, JobExecution, JobStatus};
    use crate::ota::data_interface::Protocol;
    use crate::ota::encoding::json::{FileDescription, OtaJob};
    use crate::ota::error::OtaError;
    use crate::ota::state::{Error, Events, States};
    use crate::ota::test::test_job_doc;
    use crate::ota::{
        agent::OtaAgent,
        control_interface::ControlInterface,
        data_interface::{DataInterface, NoInterface},
        pal::OtaPal,
        test::mock::{MockPal, MockTimer},
    };
    use crate::test::MockMqtt;
    use embedded_hal::timer;
    use mqttrust::encoding::v4::{decode_slice, utils::Pid, PacketType};
    use mqttrust::{MqttError, Packet, QoS, SubscribeTopic};
    use serde::Deserialize;
    use serde_json_core::from_slice;

    /// All known job document that the device knows how to process.
    #[derive(Debug, PartialEq, Deserialize)]
    pub enum JobDetails<'a> {
        #[serde(rename = "afr_ota")]
        #[serde(borrow)]
        Ota(OtaJob<'a>),

        #[serde(other)]
        Unknown,
    }

    fn new_agent(
        mqtt: &MockMqtt,
    ) -> OtaAgent<'_, MockMqtt, &MockMqtt, NoInterface, MockTimer, MockTimer, MockPal> {
        let request_timer = MockTimer::new();
        let self_test_timer = MockTimer::new();
        let pal = MockPal {};

        OtaAgent::builder(mqtt, mqtt, request_timer, pal)
            .with_self_test_timeout(self_test_timer, 16000)
            .build()
    }

    fn run_to_state<'a, C, DP, DS, T, ST, PAL>(
        agent: &mut OtaAgent<'a, C, DP, DS, T, ST, PAL>,
        state: States,
    ) where
        C: ControlInterface,
        DP: DataInterface,
        DS: DataInterface,
        T: timer::nb::CountDown + timer::nb::Cancel,
        T::Time: From<u32>,
        ST: timer::nb::CountDown + timer::nb::Cancel,
        ST::Time: From<u32>,
        PAL: OtaPal,
    {
        if agent.state.state() == &state {
            return;
        }

        match state {
            States::Ready => {
                println!(
                    "Running to 'States::Ready', events: {}",
                    agent.state.context().events.len()
                );
                agent.state.process_event(Events::Shutdown).unwrap();
            }
            States::CreatingFile => {
                println!(
                    "Running to 'States::CreatingFile', events: {}",
                    agent.state.context().events.len()
                );
                run_to_state(agent, States::WaitingForJob);

                let job_doc = test_job_doc();
                agent.job_update("Test-job", &job_doc, None).unwrap();
                agent.state.context_mut().events.dequeue();
            }
            States::RequestingFileBlock => {
                println!(
                    "Running to 'States::RequestingFileBlock', events: {}",
                    agent.state.context().events.len()
                );
                run_to_state(agent, States::CreatingFile);
                agent.state.process_event(Events::CreateFile).unwrap();
                agent.state.context_mut().events.dequeue();
            }
            States::RequestingJob => {
                println!(
                    "Running to 'States::RequestingJob', events: {}",
                    agent.state.context().events.len()
                );
                run_to_state(agent, States::Ready);
                agent.state.process_event(Events::Start).unwrap();
                agent.state.context_mut().events.dequeue();
            }
            States::Suspended => {
                println!(
                    "Running to 'States::Suspended', events: {}",
                    agent.state.context().events.len()
                );
                run_to_state(agent, States::Ready);
                agent.suspend().unwrap();
            }
            States::WaitingForFileBlock => {
                println!(
                    "Running to 'States::Suspended', events: {}",
                    agent.state.context().events.len()
                );
                run_to_state(agent, States::RequestingFileBlock);
                agent.state.process_event(Events::RequestFileBlock).unwrap();
                agent.state.context_mut().events.dequeue();
            }
            States::WaitingForJob => {
                println!(
                    "Running to 'States::WaitingForJob', events: {}",
                    agent.state.context().events.len()
                );
                run_to_state(agent, States::RequestingJob);
                agent.check_for_update().unwrap();
            }
            States::Restarting => {}
        }
    }

    pub fn set_pid(buf: &mut [u8], pid: Pid) -> Result<(), ()> {
        let mut offset = 0;
        let (header, _) = mqttrust::encoding::v4::decoder::read_header(buf, &mut offset)
            .map_err(|_| ())?
            .ok_or(())?;

        match (header.typ, header.qos) {
            (PacketType::Publish, QoS::AtLeastOnce | QoS::ExactlyOnce) => {
                if buf[offset..].len() < 2 {
                    return Err(());
                }
                let len = ((buf[offset] as usize) << 8) | buf[offset + 1] as usize;

                offset += 2;
                if len > buf[offset..].len() {
                    return Err(());
                } else {
                    offset += len;
                }
            }
            (PacketType::Subscribe | PacketType::Unsubscribe | PacketType::Suback, _) => {}
            (
                PacketType::Puback
                | PacketType::Pubrec
                | PacketType::Pubrel
                | PacketType::Pubcomp
                | PacketType::Unsuback,
                _,
            ) => {}
            _ => return Ok(()),
        }

        pid.to_buffer(buf, &mut offset).map_err(|_| ())
    }

    #[test]
    fn ready_when_stopped() {
        let mqtt = MockMqtt::new();
        let mut ota_agent = new_agent(&mqtt);

        assert!(matches!(ota_agent.state.state(), &States::Ready));
        run_to_state(&mut ota_agent, States::Ready);
        assert!(matches!(ota_agent.state.state(), &States::Ready));
        assert_eq!(ota_agent.state.context().events.len(), 0);
        assert_eq!(mqtt.tx.borrow_mut().len(), 0);
    }

    #[test]
    fn abort_when_stopped() {
        let mqtt = MockMqtt::new();
        let mut ota_agent = new_agent(&mqtt);

        run_to_state(&mut ota_agent, States::Ready);
        assert_eq!(ota_agent.state.context().events.len(), 0);

        assert_eq!(
            ota_agent.abort().err(),
            Some(Error::GuardFailed(OtaError::NoActiveJob))
        );
        ota_agent.process_event().unwrap();
        assert!(matches!(ota_agent.state.state(), &States::Ready));
        assert_eq!(mqtt.tx.borrow_mut().len(), 0);
    }

    #[test]
    fn resume_when_stopped() {
        let mqtt = MockMqtt::new();
        let mut ota_agent = new_agent(&mqtt);

        run_to_state(&mut ota_agent, States::Ready);
        assert_eq!(ota_agent.state.context().events.len(), 0);

        assert!(matches!(
            ota_agent.resume().err().unwrap(),
            Error::InvalidEvent
        ));
        ota_agent.process_event().unwrap();
        assert!(matches!(ota_agent.state.state(), &States::Ready));
        assert_eq!(mqtt.tx.borrow_mut().len(), 0);
    }

    #[test]
    fn resume_when_suspended() {
        let mqtt = MockMqtt::new();
        let mut ota_agent = new_agent(&mqtt);

        run_to_state(&mut ota_agent, States::Suspended);
        assert_eq!(ota_agent.state.context().events.len(), 0);

        assert!(matches!(
            ota_agent.resume().unwrap(),
            &States::RequestingJob
        ));
        assert_eq!(mqtt.tx.borrow_mut().len(), 0);
    }

    #[test]
    fn check_for_update() {
        let mqtt = MockMqtt::new();
        let mut ota_agent = new_agent(&mqtt);

        run_to_state(&mut ota_agent, States::RequestingJob);
        assert!(matches!(ota_agent.state.state(), &States::RequestingJob));

        assert_eq!(ota_agent.state.context().events.len(), 0);

        assert!(matches!(
            ota_agent.check_for_update().unwrap(),
            &States::WaitingForJob
        ));

        let bytes = mqtt.tx.borrow_mut().pop_front().unwrap();

        let packet = decode_slice(bytes.as_slice()).unwrap();
        let topics = match packet {
            Some(Packet::Subscribe(ref s)) => s.topics().collect::<Vec<_>>(),
            _ => panic!(),
        };

        assert_eq!(
            topics,
            vec![SubscribeTopic {
                topic_path: "$aws/things/test_client/jobs/notify-next",
                qos: QoS::AtLeastOnce
            }]
        );

        let mut bytes = mqtt.tx.borrow_mut().pop_front().unwrap();
        set_pid(bytes.as_mut_slice(), Pid::new()).expect("Failed to set valid PID");
        let packet = decode_slice(bytes.as_slice()).unwrap();

        let publish = match packet {
            Some(Packet::Publish(p)) => p,
            _ => panic!(),
        };

        assert_eq!(
            publish,
            mqttrust::encoding::v4::publish::Publish {
                dup: false,
                qos: QoS::AtLeastOnce,
                retain: false,
                topic_name: "$aws/things/test_client/jobs/$next/get",
                payload: &[
                    123, 34, 99, 108, 105, 101, 110, 116, 84, 111, 107, 101, 110, 34, 58, 34, 48,
                    58, 116, 101, 115, 116, 95, 99, 108, 105, 101, 110, 116, 34, 125
                ],
                pid: Some(Pid::new()),
            }
        );
        assert_eq!(mqtt.tx.borrow_mut().len(), 0);
    }

    #[test]
    #[ignore]
    fn request_job_retry_fail() {
        let mut mqtt = MockMqtt::new();

        // Let MQTT publish fail so request job will also fail
        mqtt.publish_fail();

        let mut ota_agent = new_agent(&mqtt);

        // Place the OTA Agent into the state for requesting a job
        run_to_state(&mut ota_agent, States::RequestingJob);
        assert!(matches!(ota_agent.state.state(), &States::RequestingJob));
        assert_eq!(ota_agent.state.context().events.len(), 0);

        assert_eq!(
            ota_agent.check_for_update().err(),
            Some(Error::GuardFailed(OtaError::Mqtt(MqttError::Full)))
        );

        // Fail the maximum number of attempts to request a job document
        for _ in 0..ota_agent.state.context().config.max_request_momentum {
            ota_agent.process_event().unwrap();
            assert!(ota_agent.state.context().request_timer.is_started);
            ota_agent.timer_callback().ok();
            assert!(matches!(ota_agent.state.state(), &States::RequestingJob));
        }

        // Attempt to request another job document after failing the maximum
        // number of times, triggering a shutdown event.
        ota_agent.process_event().unwrap();
        assert!(matches!(ota_agent.state.state(), &States::Ready));
        assert_eq!(mqtt.tx.borrow_mut().len(), 4);
    }

    #[test]
    fn init_file_transfer_mqtt() {
        let mqtt = MockMqtt::new();

        let mut ota_agent = new_agent(&mqtt);

        // Place the OTA Agent into the state for creating file
        run_to_state(&mut ota_agent, States::CreatingFile);
        assert!(matches!(ota_agent.state.state(), &States::CreatingFile));
        assert_eq!(ota_agent.state.context().events.len(), 0);

        ota_agent.process_event().unwrap();
        assert!(matches!(ota_agent.state.state(), &States::CreatingFile));
        ota_agent.process_event().unwrap();

        ota_agent.state.process_event(Events::CreateFile).unwrap();

        // Above will automatically enqueue `RequestFileBlock`
        assert!(matches!(
            ota_agent.state.state(),
            &States::RequestingFileBlock
        ));

        // Check the latest MQTT message
        let bytes = mqtt.tx.borrow_mut().pop_back().unwrap();

        let packet = decode_slice(bytes.as_slice()).unwrap();
        let topics = match packet {
            Some(Packet::Subscribe(ref s)) => s.topics().collect::<Vec<_>>(),
            _ => panic!(),
        };

        assert_eq!(
            topics,
            vec![SubscribeTopic {
                topic_path: "$aws/things/test_client/streams/test_stream/data/cbor",
                qos: QoS::AtLeastOnce
            }]
        );

        // Should still contain:
        // - subscription to `$aws/things/test_client/jobs/notify-next`
        // - publish to `$aws/things/test_client/jobs/$next/get`
        assert_eq!(mqtt.tx.borrow_mut().len(), 2);
    }

    #[test]
    fn request_file_block_mqtt() {
        let mqtt = MockMqtt::new();

        let mut ota_agent = new_agent(&mqtt);

        // Place the OTA Agent into the state for requesting file block
        run_to_state(&mut ota_agent, States::RequestingFileBlock);
        assert!(matches!(
            ota_agent.state.state(),
            &States::RequestingFileBlock
        ));
        assert_eq!(ota_agent.state.context().events.len(), 0);

        ota_agent
            .state
            .process_event(Events::RequestFileBlock)
            .unwrap();

        assert!(matches!(
            ota_agent.state.state(),
            &States::WaitingForFileBlock
        ));

        let bytes = mqtt.tx.borrow_mut().pop_back().unwrap();

        let publish = match decode_slice(bytes.as_slice()).unwrap() {
            Some(Packet::Publish(p)) => p,
            _ => panic!(),
        };

        // Check the latest MQTT message
        assert_eq!(
            publish,
            mqttrust::encoding::v4::publish::Publish {
                dup: false,
                qos: QoS::AtMostOnce,
                retain: false,
                topic_name: "$aws/things/test_client/streams/test_stream/get/cbor",
                payload: &[
                    164, 97, 102, 0, 97, 108, 25, 1, 0, 97, 111, 0, 97, 98, 68, 255, 255, 255, 127
                ],
                pid: None
            }
        );

        // Should still contain:
        // - subscription to `$aws/things/test_client/jobs/notify-next`
        // - publish to `$aws/things/test_client/jobs/$next/get`
        // - subscription to
        //   `$aws/things/test_client/streams/test_stream/data/cbor`
        assert_eq!(mqtt.tx.borrow_mut().len(), 3);
    }

    #[test]
    fn deserialize_describe_job_execution_response_ota() {
        let payload = br#"{
            "clientToken":"0:rustot-test",
            "timestamp":1624445100,
            "execution":{
                "jobId":"AFR_OTA-rustot_test_1",
                "status":"QUEUED",
                "queuedAt":1624440618,
                "lastUpdatedAt":1624440618,
                "versionNumber":1,
                "executionNumber":1,
                "jobDocument":{
                    "afr_ota":{
                        "protocols":["MQTT"],
                        "streamname":"AFR_OTA-0ba01295-9417-4ba7-9a99-4b31fb03d252",
                        "files":[{
                            "filepath":"IMG_test.jpg",
                            "filesize":2674792,
                            "fileid":0,
                            "certfile":"nope",
                            "fileType":0,
                            "sig-sha256-ecdsa":"This is my signature! Better believe it!"
                        }]
                    }
                }
            }
        }"#;

        let (response, _) =
            from_slice::<DescribeJobExecutionResponse<JobDetails>>(payload).unwrap();

        assert_eq!(
            response,
            DescribeJobExecutionResponse {
                execution: Some(JobExecution {
                    execution_number: Some(1),
                    job_document: Some(JobDetails::Ota(OtaJob {
                        protocols: heapless::Vec::from_slice(&[Protocol::Mqtt]).unwrap(),
                        streamname: "AFR_OTA-0ba01295-9417-4ba7-9a99-4b31fb03d252",
                        files: heapless::Vec::from_slice(&[FileDescription {
                            filepath: "IMG_test.jpg",
                            filesize: 2674792,
                            fileid: 0,
                            certfile: "nope",
                            update_data_url: None,
                            auth_scheme: None,
                            sha1_rsa: None,
                            sha256_rsa: None,
                            sha1_ecdsa: None,
                            sha256_ecdsa: Some(heapless::String::from(
                                "This is my signature! Better believe it!"
                            )),
                            file_type: Some(0),
                        }])
                        .unwrap(),
                    })),
                    job_id: "AFR_OTA-rustot_test_1",
                    last_updated_at: 1624440618,
                    queued_at: 1624440618,
                    status_details: None,
                    status: JobStatus::Queued,
                    version_number: 1,
                    approximate_seconds_before_timed_out: None,
                    started_at: None,
                    thing_name: None,
                }),
                timestamp: 1624445100,
                client_token: "0:rustot-test",
            }
        );
    }
}
