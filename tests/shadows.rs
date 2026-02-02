#![cfg(all(feature = "std", feature = "shadows_kv_persist"))]
#![allow(async_fn_in_trait)]
#![feature(type_alias_impl_trait)]

mod common;

use common::credentials;
use common::network::TlsNetwork;
use embassy_futures::select;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use mqttrust::transport::embedded_nal::NalTransport;
use mqttrust::{Config, DomainBroker, Publish, State, Subscribe, SubscribeTopic};
use postcard::experimental::max_size::MaxSize;
use rustot_derive::shadow_root;
use serde::{Deserialize, Serialize};
use static_cell::StaticCell;

use rustot::shadows::{FileKVStore, Shadow};

// =============================================================================
// Test Shadow Type
// =============================================================================

#[shadow_root(name = "state")]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
pub struct TestShadow {
    pub foo: u32,
    pub bar: bool,
    #[shadow_attr(report_only)]
    pub version: u32,
}

// =============================================================================
// Cloud Simulation Helpers
// =============================================================================

/// Publish a desired state update to the cloud and wait for accepted response.
async fn cloud_update_desired(
    client: &mqttrust::MqttClient<'_, NoopRawMutex>,
    thing_name: &str,
    desired_json: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let update_accepted = format!(
        "$aws/things/{}/shadow/name/state/update/accepted",
        thing_name
    );
    let update_rejected = format!(
        "$aws/things/{}/shadow/name/state/update/rejected",
        thing_name
    );
    let update_topic = format!("$aws/things/{}/shadow/name/state/update", thing_name);

    let mut sub = client
        .subscribe::<2>(
            Subscribe::builder()
                .topics(&[
                    SubscribeTopic::builder()
                        .topic_path(update_accepted.as_str())
                        .build(),
                    SubscribeTopic::builder()
                        .topic_path(update_rejected.as_str())
                        .build(),
                ])
                .build(),
        )
        .await
        .map_err(|e| format!("Subscribe error: {:?}", e))?;

    client
        .publish(
            Publish::builder()
                .topic_name(update_topic.as_str())
                .payload(desired_json)
                .build(),
        )
        .await
        .map_err(|e| format!("Publish error: {:?}", e))?;

    let msg = sub.next_message().await.ok_or("No response to update")?;

    if msg.topic_name().contains("rejected") {
        return Err(format!(
            "Update rejected: {}",
            core::str::from_utf8(msg.payload()).unwrap_or("?")
        )
        .into());
    }

    Ok(())
}

/// Get the full shadow document from the cloud.
async fn cloud_get_shadow(
    client: &mqttrust::MqttClient<'_, NoopRawMutex>,
    thing_name: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let get_accepted = format!("$aws/things/{}/shadow/name/state/get/accepted", thing_name);
    let get_rejected = format!("$aws/things/{}/shadow/name/state/get/rejected", thing_name);
    let get_topic = format!("$aws/things/{}/shadow/name/state/get", thing_name);

    let mut sub = client
        .subscribe::<2>(
            Subscribe::builder()
                .topics(&[
                    SubscribeTopic::builder()
                        .topic_path(get_accepted.as_str())
                        .build(),
                    SubscribeTopic::builder()
                        .topic_path(get_rejected.as_str())
                        .build(),
                ])
                .build(),
        )
        .await
        .map_err(|e| format!("Subscribe error: {:?}", e))?;

    client
        .publish(
            Publish::builder()
                .topic_name(get_topic.as_str())
                .payload(&[] as &[u8])
                .build(),
        )
        .await
        .map_err(|e| format!("Publish error: {:?}", e))?;

    let msg = sub.next_message().await.ok_or("No response to get")?;

    if msg.topic_name().contains("rejected") {
        return Err(format!(
            "Get rejected: {}",
            core::str::from_utf8(msg.payload()).unwrap_or("?")
        )
        .into());
    }

    let doc: serde_json::Value = serde_json::from_slice(msg.payload())?;
    Ok(doc)
}

// =============================================================================
// Integration Test
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn test_shadow_end_to_end() {
    env_logger::try_init().ok();

    log::info!("Starting shadow integration test...");

    let (thing_name, identity) = credentials::identity();
    let hostname = credentials::HOSTNAME.unwrap();

    // =========================================================================
    // Step 1: Create temp dir for FileKVStore
    // =========================================================================
    let kv = FileKVStore::temp().expect("Failed to create temp FileKVStore");

    // =========================================================================
    // Setup MQTT client
    // =========================================================================
    static NETWORK: StaticCell<TlsNetwork> = StaticCell::new();
    let network = NETWORK.init(TlsNetwork::new(hostname.to_owned(), identity));

    let broker = DomainBroker::<_, 128>::new_with_port(hostname, 8883, network).unwrap();
    let config = Config::builder()
        .client_id(thing_name.try_into().unwrap())
        .keepalive_interval(embassy_time::Duration::from_secs(50))
        .build();

    static STATE: StaticCell<State<NoopRawMutex, 4096, { 4096 * 10 }>> = StaticCell::new();
    let state = STATE.init(State::new());
    let (mut stack, client) = mqttrust::new(state, config);

    // =========================================================================
    // Test logic future
    // =========================================================================
    let test_fut = async {
        // Wait for MQTT connection to establish
        client.wait_connected().await;
        log::info!("MQTT connected");

        let shadow = Shadow::<TestShadow, _, FileKVStore>::new(&kv, &client);

        // =====================================================================
        // Step 2: Load shadow → initializes defaults, persists to file store
        // =====================================================================
        let load_result = shadow.load().await.expect("Failed to load shadow");
        log::info!("Load result: first_boot={}", load_result.first_boot);
        assert!(load_result.first_boot);

        // =====================================================================
        // Step 3: Delete shadow from cloud (ignore NotFound)
        // =====================================================================
        match shadow.delete_shadow().await {
            Ok(()) => log::info!("Deleted existing shadow from cloud"),
            Err(e) => log::info!("Delete shadow (expected if not found): {:?}", e),
        }

        // =====================================================================
        // Step 4: Sync shadow → creates shadow with defaults, subscribes to delta
        // =====================================================================
        let state_val = shadow.sync_shadow().await.expect("Failed to sync shadow");
        log::info!("Synced shadow: {:?}", state_val);

        // =====================================================================
        // Step 5: Assert defaults
        // =====================================================================
        assert_eq!(state_val.foo, 0);
        assert_eq!(state_val.bar, false);

        // =====================================================================
        // Step 6: Cloud updates desired foo=42
        // =====================================================================
        log::info!("Cloud updating desired foo=42...");
        cloud_update_desired(&client, thing_name, br#"{"state":{"desired":{"foo":42}}}"#)
            .await
            .expect("Failed to update desired foo");

        // =====================================================================
        // Step 7: Wait for delta → receives foo delta, applies, persists, acknowledges
        // =====================================================================
        log::info!("Waiting for delta...");
        let (state_val, delta) = shadow
            .wait_delta()
            .await
            .expect("Failed to wait_delta (foo)");
        log::info!(
            "Got foo delta: has_delta={}, state: {:?}",
            delta.is_some(),
            state_val
        );

        // =====================================================================
        // Step 8: Assert foo updated
        // =====================================================================
        assert_eq!(state_val.foo, 42);
        assert_eq!(state_val.bar, false);

        // =====================================================================
        // Step 9: Report version=1 to cloud (report_only field)
        // =====================================================================
        log::info!("Reporting version=1...");
        shadow
            .update(|_, r| {
                r.version = Some(1);
            })
            .await
            .expect("Failed to update reported version");

        // =====================================================================
        // Step 10: Cloud get shadow → assert reported.version == 1
        // =====================================================================
        log::info!("Getting shadow document from cloud...");
        let doc = cloud_get_shadow(&client, thing_name)
            .await
            .expect("Failed to get shadow");
        log::info!("Shadow document: {}", doc);

        let reported_version = doc["state"]["reported"]["version"]
            .as_u64()
            .expect("version not in reported state");
        assert_eq!(reported_version, 1);

        // version should NOT generate a delta (it's report_only)
        assert!(
            doc["state"]["delta"].is_null()
                || doc["state"].get("delta").is_none()
                || !doc["state"]["delta"]
                    .as_object()
                    .map(|d| d.contains_key("version"))
                    .unwrap_or(false),
            "version should not appear in delta (report_only)"
        );

        // =====================================================================
        // Step 11: Cloud updates desired bar=true
        // =====================================================================
        log::info!("Cloud updating desired bar=true...");
        cloud_update_desired(
            &client,
            thing_name,
            br#"{"state":{"desired":{"bar":true}}}"#,
        )
        .await
        .expect("Failed to update desired bar");

        // =====================================================================
        // Step 12: Wait for delta → receives bar delta
        // =====================================================================
        log::info!("Waiting for delta (bar)...");
        let (state_val, delta) = shadow
            .wait_delta()
            .await
            .expect("Failed to wait_delta (bar)");
        log::info!(
            "Got bar delta: has_delta={}, state: {:?}",
            delta.is_some(),
            state_val
        );

        // =====================================================================
        // Step 13: Assert both fields updated
        // =====================================================================
        assert_eq!(state_val.foo, 42);
        assert_eq!(state_val.bar, true);

        // =====================================================================
        // Step 14: Assert temp dir contains persisted files
        // =====================================================================
        let entries: Vec<_> = std::fs::read_dir(kv.base_path())
            .expect("Failed to read temp dir")
            .collect();
        log::info!("FileKVStore has {} files", entries.len());
        assert!(
            !entries.is_empty(),
            "FileKVStore temp dir should contain persisted files"
        );

        // =====================================================================
        // Step 15: Delete shadow → delete from cloud, reset store to defaults
        // =====================================================================
        log::info!("Deleting shadow...");
        shadow
            .delete_shadow()
            .await
            .expect("Failed to delete shadow");

        // =====================================================================
        // Step 16: Assert state loaded from store == defaults
        // =====================================================================
        let state_val = shadow
            .state()
            .await
            .expect("Failed to get state after delete");
        assert_eq!(state_val.foo, 0);
        assert_eq!(state_val.bar, false);
        assert_eq!(state_val.version, 0);

        // =====================================================================
        // Step 17: Remove temp dir, assert removal succeeded
        // =====================================================================
        let temp_path = kv.base_path().to_owned();
        std::fs::remove_dir_all(&temp_path).expect("Failed to remove temp dir");
        assert!(!temp_path.exists(), "Temp dir should be removed");

        log::info!("Shadow integration test passed!");
        Ok::<_, Box<dyn std::error::Error>>(())
    };

    // =========================================================================
    // Run MQTT stack + test concurrently with timeout
    // =========================================================================
    let mut transport = NalTransport::new(network, broker);

    match embassy_time::with_timeout(
        embassy_time::Duration::from_secs(60),
        select::select(stack.run(&mut transport), test_fut),
    )
    .await
    .expect("Test timed out after 60 seconds")
    {
        select::Either::First(_) => {
            unreachable!("MQTT stack should not terminate before test")
        }
        select::Either::Second(result) => result.unwrap(),
    };
}
