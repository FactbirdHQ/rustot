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

pub fn test_job_doc() -> OtaJob {
    OtaJob {
        protocols: heapless::Vec::from_slice(&[Protocol::Mqtt]).unwrap(),
        streamname: heapless::String::from("test_stream"),
        files: heapless::Vec::from_slice(&[FileDescription {
            filepath: heapless::String::from(""),
            filesize: 123456,
            fileid: 0,
            certfile: heapless::String::from("cert"),
            update_data_url: None,
            auth_scheme: None,
            sig_sha1_rsa: heapless::String::from(""),
            file_attributes: Some(0),
        }])
        .unwrap(),
    }
}

pub fn test_file_ctx(config: &Config) -> FileContext {
    let ota_job = test_job_doc();
    FileContext::new_from(&ota_job, None, 0, config, Version::default()).unwrap()
}

pub mod ota_tests {
    use crate::ota::state::{Error, Events, States};
    use crate::ota::test::mock::{MqttRequest, OwnedPublishRequest};
    use crate::ota::test::test_job_doc;
    use crate::ota::{
        agent::OtaAgent,
        control_interface::ControlInterface,
        data_interface::{DataInterface, NoInterface},
        pal::OtaPal,
        test::mock::{MockMqtt, MockPal, MockTimer},
    };
    use embedded_hal::timer;
    use mqttrust::{QoS, SubscribeRequest, SubscribeTopic};

    fn new_agent(
        mqtt: &MockMqtt,
    ) -> OtaAgent<'_, MockMqtt, &MockMqtt, NoInterface, MockTimer, MockPal> {
        let request_timer = MockTimer::new();
        let pal = MockPal {};

        OtaAgent::builder(mqtt, mqtt, request_timer, pal).build()
    }

    fn run_to_state<'a, C, DP, DS, T, PAL>(
        agent: &mut OtaAgent<'a, C, DP, DS, T, PAL>,
        state: States,
    ) where
        C: ControlInterface,
        DP: DataInterface,
        DS: DataInterface,
        T: timer::CountDown + timer::Cancel,
        T::Time: From<u32>,
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
                agent.job_update(job_doc, None).unwrap();
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
        }
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

        assert!(matches!(
            ota_agent.abort().err().unwrap(),
            Error::GuardFailed
        ));
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

        assert_eq!(
            mqtt.tx.borrow_mut().pop_front(),
            Some(MqttRequest::Subscribe(SubscribeRequest {
                topics: heapless::Vec::from_slice(&[SubscribeTopic {
                    topic_path: heapless::String::from("$aws/things/test_client/jobs/notify-next"),
                    qos: QoS::AtLeastOnce
                }])
                .unwrap()
            }))
        );

        assert_eq!(
            mqtt.tx.borrow_mut().pop_front(),
            Some(MqttRequest::Publish(OwnedPublishRequest {
                dup: false,
                qos: QoS::AtLeastOnce,
                retain: false,
                topic_name: String::from("$aws/things/test_client/jobs/$next/get"),
                payload: vec![
                    123, 34, 99, 108, 105, 101, 110, 116, 84, 111, 107, 101, 110, 34, 58, 34, 48,
                    58, 116, 101, 115, 116, 95, 99, 108, 105, 101, 110, 116, 34, 125
                ]
            }))
        );
        assert_eq!(mqtt.tx.borrow_mut().len(), 0);
    }

    #[test]
    fn request_job_retry_fail() {
        let mut mqtt = MockMqtt::new();

        // Let MQTT publish fail so request job will also fail
        mqtt.publish_fail();

        let mut ota_agent = new_agent(&mqtt);

        // Place the OTA Agent into the state for requesting a job
        run_to_state(&mut ota_agent, States::RequestingJob);
        assert!(matches!(ota_agent.state.state(), &States::RequestingJob));
        assert_eq!(ota_agent.state.context().events.len(), 0);

        assert!(matches!(
            ota_agent.check_for_update().err(),
            Some(Error::GuardFailed)
        ));

        // Fail the maximum number of attempts to request a job document
        for _ in 0..ota_agent.state.context().config.max_request_momentum {
            ota_agent.process_event().ok();
            assert!(ota_agent.state.context().request_timer.is_started);
            ota_agent.timer_callback();
            // assert!(!ota_agent.state.context().request_timer.is_started);
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
        assert_eq!(
            mqtt.tx.borrow_mut().pop_back(),
            Some(MqttRequest::Subscribe(SubscribeRequest {
                topics: heapless::Vec::from_slice(&[SubscribeTopic {
                    topic_path: heapless::String::from(
                        "$aws/things/test_client/streams/test_stream/data/cbor"
                    ),
                    qos: QoS::AtLeastOnce
                }])
                .unwrap()
            }))
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

        // Check the latest MQTT message
        assert_eq!(
            mqtt.tx.borrow_mut().pop_back(),
            Some(MqttRequest::Publish(OwnedPublishRequest {
                dup: false,
                qos: QoS::AtMostOnce,
                retain: false,
                topic_name: String::from("$aws/things/test_client/streams/test_stream/get/cbor"),
                payload: vec![
                    164, 97, 102, 0, 97, 108, 25, 1, 0, 97, 111, 0, 97, 98, 68, 255, 255, 255, 127
                ]
            }))
        );

        // Should still contain:
        // - subscription to `$aws/things/test_client/jobs/notify-next`
        // - publish to `$aws/things/test_client/jobs/$next/get`
        // - subscription to
        //   `$aws/things/test_client/streams/test_stream/data/cbor`
        assert_eq!(mqtt.tx.borrow_mut().len(), 3);
    }
}
