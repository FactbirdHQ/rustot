#![cfg(all(feature = "shadows_kv_persist", feature = "std"))]
#![allow(async_fn_in_trait)]
#![feature(type_alias_impl_trait)]

mod common;

use common::credentials;
use common::network::TlsNetwork;
use embassy_futures::select;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_mqtt::transport::embedded_nal::NalTransport;
use embedded_mqtt::{Config, DomainBroker, Publish, State, Subscribe, SubscribeTopic};
use postcard::experimental::max_size::MaxSize;
use rustot_derive::{shadow_node, shadow_root};
use serde::{Deserialize, Serialize};
use static_cell::StaticCell;

use rustot::shadows::{FileKVStore, Shadow};

// =============================================================================
// Test Shadow Types
// =============================================================================

/// Nested struct field
#[shadow_node]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
pub struct Inner {
    pub value: u32,
    pub flag: bool,
}

/// Config for internally-tagged single variant
#[shadow_node]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
pub struct SingleConfig {
    pub x: u8,
}

/// Config for internally-tagged pair variant
#[shadow_node]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
pub struct PairConfig {
    pub x: u8,
    pub y: u8,
}

/// Regular internally-tagged enum with data variants
#[shadow_node]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaggedEnum {
    #[default]
    None,
    Single(SingleConfig),
    Pair(PairConfig),
}

/// Config for variant A of the adjacently-tagged enum
#[shadow_node]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
pub struct ConfigA {
    pub alpha: bool,
    pub beta: u16,
}

/// Config for variant B of the adjacently-tagged enum
#[shadow_node]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
pub struct ConfigB {
    pub gamma: u32,
    pub delta: u16,
}

/// Adjacently-tagged enum - tests variant fallback when tag is missing
#[shadow_node]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
#[serde(tag = "mode", content = "config", rename_all = "snake_case")]
pub enum Adjacent {
    #[default]
    Off,
    ModeA(ConfigA),
    ModeB(ConfigB),
}

/// Main test shadow covering all field types
#[shadow_root(name = "state")]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
pub struct TestShadow {
    /// Primitive fields
    pub count: u32,
    pub active: bool,

    /// Nested struct
    pub inner: Inner,

    /// Regular internally-tagged enum
    pub tagged: TaggedEnum,

    /// Adjacently-tagged enum (key test: config-only delta uses variant fallback)
    pub adjacent: Adjacent,

    /// Report-only field
    #[shadow_attr(report_only)]
    pub version: u32,
}

// =============================================================================
// Cloud Simulation Helpers
// =============================================================================

/// Publish a desired state update to the cloud and wait for accepted response.
async fn cloud_update_desired(
    client: &embedded_mqtt::MqttClient<'_, NoopRawMutex>,
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
    client: &embedded_mqtt::MqttClient<'_, NoopRawMutex>,
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

    let broker =
        DomainBroker::<_, 128>::new(format!("{}:8883", hostname).as_str(), network).unwrap();
    let config = Config::builder()
        .client_id(thing_name.try_into().unwrap())
        .keepalive_interval(embassy_time::Duration::from_secs(50))
        .build();

    static STATE: StaticCell<State<NoopRawMutex, 4096, { 4096 * 10 }>> = StaticCell::new();
    let state = STATE.init(State::new());
    let (mut stack, client) = embedded_mqtt::new(state, config);

    // =========================================================================
    // Test logic future
    // =========================================================================
    let test_fut = async {
        // Wait for MQTT connection to establish
        client.wait_connected().await;
        log::info!("MQTT connected");

        let shadow = Shadow::<TestShadow, _, FileKVStore>::new(&kv, &client);

        // =====================================================================
        // Setup: Load and sync shadow
        // =====================================================================
        let load_result = shadow.load().await.expect("Failed to load shadow");
        log::info!("Load result: first_boot={}", load_result.first_boot);
        assert!(load_result.first_boot);

        // Delete any existing shadow from previous test runs
        match shadow.delete_shadow().await {
            Ok(()) => log::info!("Deleted existing shadow from cloud"),
            Err(e) => log::info!("Delete shadow (expected if not found): {:?}", e),
        }

        // Sync creates shadow with defaults
        let state = shadow.sync_shadow().await.expect("Failed to sync shadow");
        log::info!("Synced shadow: {:?}", state);

        // Assert defaults
        assert_eq!(state.count, 0);
        assert_eq!(state.active, false);
        assert_eq!(state.inner, Inner::default());
        assert_eq!(state.tagged, TaggedEnum::None);
        assert_eq!(state.adjacent, Adjacent::Off);

        // =====================================================================
        // Test 1: Primitive fields (count, active)
        // =====================================================================
        log::info!("Test 1: Updating primitive fields...");
        cloud_update_desired(
            &client,
            thing_name,
            br#"{"state":{"desired":{"count":42,"active":true}}}"#,
        )
        .await
        .expect("Failed to update primitives");

        let (state, delta) = shadow.wait_delta().await.expect("Failed to wait_delta");
        assert!(delta.is_some());
        assert_eq!(state.count, 42);
        assert_eq!(state.active, true);
        log::info!("Test 1 passed: primitives updated");

        // =====================================================================
        // Test 2: Nested struct (inner)
        // =====================================================================
        log::info!("Test 2: Updating nested struct...");
        cloud_update_desired(
            &client,
            thing_name,
            br#"{"state":{"desired":{"inner":{"value":100,"flag":true}}}}"#,
        )
        .await
        .expect("Failed to update nested struct");

        let (state, delta) = shadow.wait_delta().await.expect("Failed to wait_delta");
        assert!(delta.is_some());
        assert_eq!(state.inner.value, 100);
        assert_eq!(state.inner.flag, true);
        log::info!("Test 2 passed: nested struct updated");

        // =====================================================================
        // Test 3: Regular internally-tagged enum
        // =====================================================================
        log::info!("Test 3: Updating internally-tagged enum...");
        cloud_update_desired(
            &client,
            thing_name,
            br#"{"state":{"desired":{"tagged":{"kind":"pair","x":10,"y":20}}}}"#,
        )
        .await
        .expect("Failed to update tagged enum");

        let (state, delta) = shadow.wait_delta().await.expect("Failed to wait_delta");
        assert!(delta.is_some());
        assert_eq!(state.tagged, TaggedEnum::Pair(PairConfig { x: 10, y: 20 }));
        log::info!("Test 3 passed: internally-tagged enum updated");

        // =====================================================================
        // Test 4: Adjacently-tagged enum - mode + config together
        // =====================================================================
        log::info!("Test 4: Updating adjacently-tagged enum (mode + config)...");
        cloud_update_desired(
            &client,
            thing_name,
            br#"{"state":{"desired":{"adjacent":{"mode":"mode_a","config":{"alpha":true,"beta":500}}}}}"#,
        )
        .await
        .expect("Failed to update adjacent enum");

        let (state, delta) = shadow.wait_delta().await.expect("Failed to wait_delta");
        assert!(delta.is_some());
        assert_eq!(
            state.adjacent,
            Adjacent::ModeA(ConfigA {
                alpha: true,
                beta: 500
            })
        );
        log::info!("Test 4 passed: adjacently-tagged enum with mode+config");

        // =====================================================================
        // Test 5: Adjacently-tagged enum - config only (variant fallback)
        // This is the key test: cloud sends only config, device uses current mode
        // =====================================================================
        log::info!("Test 5: Updating adjacently-tagged config only (variant fallback)...");
        cloud_update_desired(
            &client,
            thing_name,
            // Note: no "mode" field, only "config" - device must use current mode (mode_a)
            br#"{"state":{"desired":{"adjacent":{"config":{"alpha":false,"beta":999}}}}}"#,
        )
        .await
        .expect("Failed to update adjacent config only");

        let (state, delta) = shadow.wait_delta().await.expect("Failed to wait_delta");
        assert!(delta.is_some());
        // Should still be ModeA, with updated config
        assert_eq!(
            state.adjacent,
            Adjacent::ModeA(ConfigA {
                alpha: false,
                beta: 999
            })
        );
        log::info!("Test 5 passed: config-only delta used variant fallback");

        // =====================================================================
        // Test 6: Adjacently-tagged enum - switch to different mode
        // =====================================================================
        log::info!("Test 6: Switching adjacently-tagged to different mode...");
        cloud_update_desired(
            &client,
            thing_name,
            br#"{"state":{"desired":{"adjacent":{"mode":"mode_b","config":{"gamma":12345,"delta":42}}}}}"#,
        )
        .await
        .expect("Failed to switch adjacent mode");

        let (state, delta) = shadow.wait_delta().await.expect("Failed to wait_delta");
        assert!(delta.is_some());
        assert_eq!(
            state.adjacent,
            Adjacent::ModeB(ConfigB {
                gamma: 12345,
                delta: 42
            })
        );
        log::info!("Test 6 passed: switched to different mode");

        // =====================================================================
        // Test 7: Report-only field
        // =====================================================================
        log::info!("Test 7: Reporting version (report_only)...");
        shadow
            .update(|_, r| {
                r.version = Some(1);
            })
            .await
            .expect("Failed to report version");

        let doc = cloud_get_shadow(&client, thing_name)
            .await
            .expect("Failed to get shadow");

        let reported_version = doc["state"]["reported"]["version"]
            .as_u64()
            .expect("version not in reported state");
        assert_eq!(reported_version, 1);

        // version should NOT generate a delta (it's report_only)
        let has_version_delta = doc["state"]["delta"]
            .as_object()
            .map(|d| d.contains_key("version"))
            .unwrap_or(false);
        assert!(!has_version_delta, "version should not appear in delta");
        log::info!("Test 7 passed: report_only field works");

        // =====================================================================
        // Verify persistence
        // =====================================================================
        let entries: Vec<_> = std::fs::read_dir(kv.base_path())
            .expect("Failed to read temp dir")
            .collect();
        log::info!("FileKVStore has {} files", entries.len());
        assert!(
            !entries.is_empty(),
            "FileKVStore should have persisted files"
        );

        // =====================================================================
        // Cleanup
        // =====================================================================
        log::info!("Cleaning up...");
        shadow
            .delete_shadow()
            .await
            .expect("Failed to delete shadow");

        let state = shadow
            .state()
            .await
            .expect("Failed to get state after delete");
        assert_eq!(state, TestShadow::default());

        let temp_path = kv.base_path().to_owned();
        std::fs::remove_dir_all(&temp_path).expect("Failed to remove temp dir");
        assert!(!temp_path.exists(), "Temp dir should be removed");

        log::info!("All shadow integration tests passed!");
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
