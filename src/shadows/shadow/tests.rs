//! Tests for Shadow KV persistence

use super::ShadowTestOnly;
use crate::shadows::store::{FileKVStore, InMemory, KVStore};

// Use ShadowTestOnly for storage-only testing (doesn't require MQTT)
type Shadow<'a, S, K> = ShadowTestOnly<'a, S, K>;

// =========================================================================
// Phase 8 Tests - Test fixtures using proc macros
// =========================================================================
//
// The derive macros now use `proc-macro-crate` to automatically detect whether
// they're used inside the rustot crate (generating `crate::` paths) or externally
// (generating `::rustot::` paths).

use crate::shadows::ShadowNode;
use postcard::experimental::max_size::MaxSize;
use rustot_derive::shadow_root;
use serde::{Deserialize, Serialize};

// Test fixture: SimpleConfig (shadow name = "device")
#[shadow_root(name = "device")]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
struct SimpleConfig {
    value: u32,
}

// Test fixture: NetworkShadow (shadow name = "network")
#[shadow_root(name = "network")]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
struct NetworkShadow {
    value: u32,
}

// =========================================================================
// Phase 5 Migration Test Fixtures
// =========================================================================

use rustot_derive::shadow_node;

// Config with migration from old key
#[shadow_root(name = "device")]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
struct MigratedConfig {
    #[shadow_attr(migrate(from = "/old_timeout"))]
    timeout: u32,
}

// Old config (for rollback tests) - simulates old firmware
#[shadow_root(name = "device")]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
struct OldConfig {
    old_timeout: u32,
}

// Current config (for orphan tests)
#[shadow_root(name = "device")]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
struct CurrentConfig {
    timeout: u32,
}

// =========================================================================
// Phase 6 Enum Test Fixtures
// =========================================================================

#[shadow_node]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
struct StaticIpConfig {
    #[shadow_attr(opaque)]
    address: [u8; 4],
    #[shadow_attr(opaque)]
    gateway: [u8; 4],
}

#[shadow_node]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
enum IpSettings {
    #[default]
    Dhcp,
    Static(StaticIpConfig),
}

#[shadow_root(name = "wifi")]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
struct WifiConfig {
    ip: IpSettings,
}

// For serde rename test
#[shadow_node]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
struct WpaConfig {
    psk_length: u8,
}

#[shadow_node]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
enum WifiAuth {
    #[serde(rename = "none")]
    #[default]
    Open,
    Wpa2(WpaConfig),
}

#[shadow_root(name = "wifi")]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
struct AuthConfig {
    auth: WifiAuth,
}

// =========================================================================
// Phase 7 Delta Apply/Save Test Fixtures
// =========================================================================

#[shadow_root(name = "test")]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
struct TestConfig {
    timeout: u32,
    enabled: bool,
    retries: u8,
}

#[shadow_node]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
struct InnerConfig {
    timeout: u32,
    retries: u8,
}

#[shadow_root(name = "device")]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
struct DeviceShadow {
    config: InnerConfig,
    version: u32,
}

// =========================================================================
// Phase 8 Type Conversion Migration Test Fixtures
// =========================================================================

use crate::shadows::MigrationError;

// Conversion: u8 -> u16
fn u8_to_u16(old: &[u8], new: &mut [u8]) -> Result<usize, MigrationError> {
    let val: u8 = postcard::from_bytes(old).map_err(|_| MigrationError::InvalidSourceFormat)?;
    let serialized =
        postcard::to_slice(&(val as u16), new).map_err(|_| MigrationError::ConversionFailed)?;
    Ok(serialized.len())
}

// Conversion: ms (u32) -> secs (u32)
fn ms_to_secs(old: &[u8], new: &mut [u8]) -> Result<usize, MigrationError> {
    let ms: u32 = postcard::from_bytes(old).map_err(|_| MigrationError::InvalidSourceFormat)?;
    let secs = ms / 1000;
    let serialized =
        postcard::to_slice(&secs, new).map_err(|_| MigrationError::ConversionFailed)?;
    Ok(serialized.len())
}

// Conversion that always fails
fn failing_convert(_: &[u8], _: &mut [u8]) -> Result<usize, MigrationError> {
    Err(MigrationError::ConversionFailed)
}

// Fixture: Type conversion u8 -> u16
#[shadow_root(name = "device")]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
struct TypeMigratedConfig {
    #[shadow_attr(migrate(from = "/precision", convert = u8_to_u16))]
    precision: u16,
}

// Fixture: ms to secs conversion
#[shadow_root(name = "device")]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
struct MsToSecsConfig {
    #[shadow_attr(migrate(from = "/timeout_ms", convert = ms_to_secs))]
    timeout_secs: u32,
}

// Fixture: Failing conversion
#[shadow_root(name = "device")]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
struct FailingConversionConfig {
    #[shadow_attr(migrate(from = "/old_value", convert = failing_convert))]
    value: u16,
}

// Fixture: Multiple migration sources
#[shadow_root(name = "device")]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
struct MultiSourceConfig {
    #[shadow_attr(migrate(from = "/old_value"), migrate(from = "/legacy_value"))]
    current_value: u32,
}

// Helper functions for tests - will be used when Phase 8 tests are enabled
#[allow(dead_code)]
fn encode<T: serde::Serialize>(val: T) -> Vec<u8> {
    let mut buf = [0u8; 256];
    let slice = postcard::to_slice(&val, &mut buf).unwrap();
    slice.to_vec()
}

#[allow(dead_code)]
async fn setup_kv(entries: &[(&str, &[u8])]) -> FileKVStore {
    let kv = FileKVStore::temp().unwrap();
    for (key, value) in entries {
        kv.store(key, value).await.unwrap();
    }
    kv
}

#[allow(dead_code)]
fn empty_kv() -> FileKVStore {
    FileKVStore::temp().unwrap()
}

#[allow(dead_code)]
async fn kv_has_key(kv: &FileKVStore, key: &str) -> bool {
    let mut buf = [0u8; 256];
    kv.fetch(key, &mut buf).await.unwrap().is_some()
}

#[allow(dead_code)]
async fn kv_fetch_decode<T: serde::de::DeserializeOwned>(kv: &FileKVStore, key: &str) -> T {
    let mut buf = [0u8; 256];
    let data = kv.fetch(key, &mut buf).await.unwrap().unwrap();
    postcard::from_bytes(data).unwrap()
}

// =========================================================================
// Schema Hash Change Detection Tests
// =========================================================================

#[tokio::test]
async fn test_load_detects_schema_change() {
    // Store a different schema hash (simulates old firmware)
    let kv = setup_kv(&[
        ("device/__schema_hash__", &0xDEADBEEFu64.to_le_bytes()),
        ("device/value", &encode(42u32)),
    ])
    .await;

    // Shadow borrows KVStore via & reference (interior mutability)
    let shadow = Shadow::<SimpleConfig, _>::new(&kv);
    let result = shadow.load().await.unwrap();

    assert!(!result.first_boot); // Not first boot - hash existed
    assert!(result.schema_changed); // But hash didn't match
}

#[tokio::test]
async fn test_load_no_schema_change_when_hash_matches() {
    let kv = setup_kv(&[
        (
            "device/__schema_hash__",
            &SimpleConfig::SCHEMA_HASH.to_le_bytes(),
        ),
        ("device/value", &encode(42u32)),
    ])
    .await;

    let shadow = Shadow::<SimpleConfig, _>::new(&kv);
    let result = shadow.load().await.unwrap();

    assert!(!result.first_boot);
    assert!(!result.schema_changed);
    assert_eq!(shadow.state().await.unwrap().value, 42);
}

#[tokio::test]
async fn test_first_boot_initializes_and_persists() {
    // Empty store = first boot
    let kv = empty_kv();

    let shadow = Shadow::<SimpleConfig, _>::new(&kv);
    let result = shadow.load().await.unwrap();

    assert!(result.first_boot); // First boot detected
    assert!(!result.schema_changed); // Not a schema change - it's first boot

    // Verify hash was written
    let hash_key = "device/__schema_hash__";
    assert!(kv_has_key(&kv, hash_key).await);

    // Verify fields were persisted with defaults
    let value: u32 = kv_fetch_decode(&kv, "device/value").await;
    assert_eq!(value, SimpleConfig::default().value);
}

#[tokio::test]
async fn test_first_boot_then_normal_boot() {
    let kv = empty_kv();

    // First boot
    {
        let shadow1 = Shadow::<SimpleConfig, _>::new(&kv);
        let result1 = shadow1.load().await.unwrap();
        assert!(result1.first_boot);
    }

    // Second boot - should be normal load
    {
        let shadow2 = Shadow::<SimpleConfig, _>::new(&kv);
        let result2 = shadow2.load().await.unwrap();
        assert!(!result2.first_boot);
        assert!(!result2.schema_changed);
    }
}

// =========================================================================
// Multiple Shadows Sharing Same KVStore Tests
// =========================================================================

#[tokio::test]
async fn test_multiple_shadows_share_kvstore() {
    let kv = empty_kv();

    // Multiple shadows can share the same KVStore via & references
    // This is the key benefit of interior mutability
    let device = Shadow::<SimpleConfig, _>::new(&kv);
    let network = Shadow::<NetworkShadow, _>::new(&kv);

    // Both initialize on first boot (different prefixes, same KVStore)
    device.load().await.unwrap();
    network.load().await.unwrap();

    // Modify via apply_and_save (Phase 7)
    let delta = DeltaSimpleConfig { value: Some(42) };
    device.apply_and_save(&delta).await.unwrap();

    let delta = DeltaNetworkShadow { value: Some(99) };
    network.apply_and_save(&delta).await.unwrap();

    // Reload fresh - still sharing the same KVStore
    let device2 = Shadow::<SimpleConfig, _>::new(&kv);
    let network2 = Shadow::<NetworkShadow, _>::new(&kv);

    device2.load().await.unwrap();
    network2.load().await.unwrap();

    assert_eq!(device2.state().await.unwrap().value, 42);
    assert_eq!(network2.state().await.unwrap().value, 99);
}

// =========================================================================
// Non-Persisted Shadow (InMemory) Tests
// =========================================================================

#[tokio::test]
async fn test_in_memory_shadow_initializes_defaults() {
    // Non-persisted shadow using InMemory KVStore
    let kv = InMemory::<SimpleConfig>::new();
    let shadow = Shadow::<SimpleConfig, _>::new(&kv);
    let result = shadow.load().await.unwrap();

    // First boot since InMemory starts with defaults
    assert!(result.first_boot);
    assert_eq!(shadow.state().await.unwrap(), SimpleConfig::default());
}

#[tokio::test]
async fn test_in_memory_shadow_apply_and_save_updates_state() {
    let kv = InMemory::<SimpleConfig>::new();
    let shadow = Shadow::<SimpleConfig, _>::new(&kv);
    shadow.load().await.unwrap();

    let delta = DeltaSimpleConfig { value: Some(42) };
    shadow.apply_and_save(&delta).await.unwrap();

    // State updated in InMemory
    assert_eq!(shadow.state().await.unwrap().value, 42);
}

#[tokio::test]
async fn test_in_memory_shadow_persists_across_shadow_instances() {
    // InMemory now persists state within the same KVStore instance
    let kv = InMemory::<SimpleConfig>::new();

    {
        let shadow = Shadow::<SimpleConfig, _>::new(&kv);
        shadow.load().await.unwrap();

        let delta = DeltaSimpleConfig { value: Some(42) };
        shadow.apply_and_save(&delta).await.unwrap();
    }

    // Create new shadow pointing to same KVStore - state persists
    let shadow2 = Shadow::<SimpleConfig, _>::new(&kv);
    shadow2.load().await.unwrap();

    assert_eq!(shadow2.state().await.unwrap().value, 42);
}

#[tokio::test]
async fn test_new_in_memory_kvstore_has_default_state() {
    // New InMemory KVStore starts with default state
    let kv = InMemory::<SimpleConfig>::new();
    let shadow = Shadow::<SimpleConfig, _>::new(&kv);
    shadow.load().await.unwrap();

    assert_eq!(
        shadow.state().await.unwrap().value,
        SimpleConfig::default().value
    );
}

// =========================================================================
// Phase 5 Tests: Migration Logic
// =========================================================================

// =========================================================================
// 5.1 Migration Source Resolution
// =========================================================================

#[tokio::test]
async fn test_migration_prefers_primary_key_over_old() {
    // Both old and new keys exist - must use new key
    let kv = setup_kv(&[
        (
            "device/__schema_hash__",
            &MigratedConfig::SCHEMA_HASH.to_le_bytes(),
        ),
        ("device/timeout", &encode(5000u32)),
        ("device/old_timeout", &encode(9999u32)),
    ])
    .await;

    let shadow = Shadow::<MigratedConfig, _>::new(&kv);
    shadow.load().await.unwrap();

    assert_eq!(shadow.state().await.unwrap().timeout, 5000); // NOT 9999
}

#[tokio::test]
async fn test_migration_multiple_sources_tried_in_order() {
    // Only second source exists (old_value doesn't exist, legacy_value does)
    let kv = setup_kv(&[
        ("device/__schema_hash__", &0xDEADBEEFu64.to_le_bytes()),
        ("device/legacy_value", &encode(42u32)),
        // "old_value" doesn't exist
        // "current_value" doesn't exist
    ])
    .await;

    let shadow = Shadow::<MultiSourceConfig, _>::new(&kv);
    shadow.load().await.unwrap();

    assert_eq!(shadow.state().await.unwrap().current_value, 42);
}

#[tokio::test]
async fn test_migration_type_conversion_when_new_type_fails_deserialize() {
    // Key exists at migration source with old type (u8), field expects new type (u16)
    // Conversion function converts u8 -> u16
    let kv = setup_kv(&[
        ("device/__schema_hash__", &0xDEADBEEFu64.to_le_bytes()),
        ("device/precision", &encode(42u8)), // old type at migration source
    ])
    .await;

    let shadow = Shadow::<TypeMigratedConfig, _>::new(&kv);
    shadow.load().await.unwrap();

    assert_eq!(shadow.state().await.unwrap().precision, 42u16); // converted
}

// =========================================================================
// 5.2 OTA-Safe Two-Phase Behavior
// =========================================================================

#[tokio::test]
async fn test_load_writes_new_key_but_preserves_old() {
    // Set up with old schema hash to trigger migration
    let kv = setup_kv(&[
        ("device/__schema_hash__", &0xDEADBEEFu64.to_le_bytes()),
        ("device/old_timeout", &encode(5000u32)),
    ])
    .await;

    let shadow = Shadow::<MigratedConfig, _>::new(&kv);
    let result = shadow.load().await.unwrap();

    assert_eq!(result.fields_migrated, 1);

    // New key written
    assert!(kv_has_key(&kv, "device/timeout").await);
    // Old key STILL exists (for rollback)
    assert!(kv_has_key(&kv, "device/old_timeout").await);
}

#[tokio::test]
async fn test_commit_removes_old_keys_only_after_explicit_call() {
    // Set up with old schema hash to trigger migration
    let kv = setup_kv(&[
        ("device/__schema_hash__", &0xDEADBEEFu64.to_le_bytes()),
        ("device/old_timeout", &encode(5000u32)),
    ])
    .await;

    let shadow = Shadow::<MigratedConfig, _>::new(&kv);
    shadow.load().await.unwrap();

    // Old key still there
    assert!(kv_has_key(&kv, "device/old_timeout").await);

    // Now commit (after boot marked successful)
    shadow.commit().await.unwrap();

    // Old key gone
    assert!(!kv_has_key(&kv, "device/old_timeout").await);
    // New key still there
    assert!(kv_has_key(&kv, "device/timeout").await);
}

#[tokio::test]
async fn test_rollback_scenario_old_firmware_reads_old_keys() {
    // Simulate: new firmware migrated, then rollback to old firmware
    let kv = setup_kv(&[
        (
            "device/__schema_hash__",
            &OldConfig::SCHEMA_HASH.to_le_bytes(),
        ),
        ("device/timeout", &encode(5000u32)), // new key (from migration)
        ("device/old_timeout", &encode(5000u32)), // old key (preserved)
    ])
    .await;

    // "Old firmware" only knows about old_timeout
    let old_shadow = Shadow::<OldConfig, _>::new(&kv);
    old_shadow.load().await.unwrap();

    assert_eq!(old_shadow.state().await.unwrap().old_timeout, 5000); // Works!
}

// =========================================================================
// 5.3 Type Conversion Edge Cases
// =========================================================================

#[tokio::test]
async fn test_conversion_function_receives_correct_bytes() {
    // Verify the conversion wrapper correctly deserializes old type,
    // calls user function, and serializes new type
    let kv = setup_kv(&[
        ("device/__schema_hash__", &0xDEADBEEFu64.to_le_bytes()),
        ("device/timeout_ms", &encode(5000u32)),
    ])
    .await;

    let shadow = Shadow::<MsToSecsConfig, _>::new(&kv);
    shadow.load().await.unwrap();

    assert_eq!(shadow.state().await.unwrap().timeout_secs, 5); // 5000ms -> 5s
}

#[tokio::test]
async fn test_conversion_failure_propagates_error() {
    // Conversion function always fails - error should propagate
    let kv = setup_kv(&[
        ("device/__schema_hash__", &0xDEADBEEFu64.to_le_bytes()),
        ("device/old_value", &encode(123u32)), // Value at migration source (not primary key)
    ])
    .await;

    let shadow = Shadow::<FailingConversionConfig, _>::new(&kv);
    let result = shadow.load().await;

    assert!(result.is_err());
}

// =========================================================================
// 5.4 Commit Orphan Detection
// =========================================================================

#[tokio::test]
async fn test_commit_removes_truly_orphaned_keys() {
    // Simulate: old schema had "device/removed_field", new schema doesn't
    let kv = setup_kv(&[
        (
            "device/__schema_hash__",
            &0xDEADBEEFu64.to_le_bytes(), // Different hash to trigger migration
        ),
        ("device/timeout", &encode(5000u32)), // current field
        ("device/removed_field", &encode(123u32)), // orphaned - not in schema
        ("device/also_removed", &encode(456u32)), // orphaned - not in schema
    ])
    .await;

    let shadow = Shadow::<CurrentConfig, _>::new(&kv);
    shadow.load().await.unwrap();
    shadow.commit().await.unwrap();

    // Current field preserved
    assert!(kv_has_key(&kv, "device/timeout").await);
    // Orphaned keys removed
    assert!(!kv_has_key(&kv, "device/removed_field").await);
    assert!(!kv_has_key(&kv, "device/also_removed").await);
}

#[tokio::test]
async fn test_commit_preserves_inactive_enum_variant_fields() {
    // Inactive variant fields are NOT orphans - they should be preserved during commit
    let kv = setup_kv(&[
        ("wifi/__schema_hash__", &WifiCfg::SCHEMA_HASH.to_le_bytes()),
        ("wifi/ip/_variant", b"Dhcp"),
        ("wifi/ip/Static/address", &encode([192u8, 168, 1, 100])),
        ("wifi/ip/Static/gateway", &encode([192u8, 168, 1, 1])),
    ])
    .await;

    let shadow = Shadow::<WifiCfg, _>::new(&kv);
    shadow.load().await.unwrap();

    // Verify state is Dhcp
    assert!(matches!(
        shadow.state().await.unwrap().ip,
        IpSettingsCfg::Dhcp
    ));

    // Commit should NOT remove the Static variant fields
    shadow.commit().await.unwrap();

    // Static fields should still exist (they're valid keys, not orphans)
    assert!(kv_has_key(&kv, "wifi/ip/Static/address").await);
    assert!(kv_has_key(&kv, "wifi/ip/Static/gateway").await);
}

#[tokio::test]
async fn test_commit_does_not_affect_other_shadow_prefixes() {
    // GC for "device" should not touch "network" keys
    let kv = setup_kv(&[
        (
            "device/__schema_hash__",
            &0xDEADBEEFu64.to_le_bytes(), // Different hash to trigger migration
        ),
        ("device/timeout", &encode(5000u32)),
        ("network/some_key", &encode(123u32)),
    ])
    .await;

    let shadow = Shadow::<CurrentConfig, _>::new(&kv);
    shadow.load().await.unwrap();
    shadow.commit().await.unwrap();

    // Network keys untouched
    assert!(kv_has_key(&kv, "network/some_key").await);
}

// =========================================================================
// Phase 6 Tests: Enum Handling
// =========================================================================

// Phase 6 test fixtures - using different names to avoid conflict with earlier tests

#[shadow_node]
#[derive(Clone, Default, Serialize, Deserialize, MaxSize)]
struct StaticIpCfg {
    #[shadow_attr(opaque)]
    address: [u8; 4],
    #[shadow_attr(opaque)]
    gateway: [u8; 4],
}

#[shadow_node]
#[derive(Clone, Default, Serialize, Deserialize, MaxSize)]
enum IpSettingsCfg {
    #[default]
    Dhcp,
    Static(StaticIpCfg),
}

#[shadow_root(name = "wifi")]
#[derive(Clone, Default, Serialize, Deserialize, MaxSize)]
struct WifiCfg {
    ip: IpSettingsCfg,
}

// Helper to fetch raw bytes from KV (not postcard-decoded)
#[allow(dead_code)]
async fn kv_fetch_raw(kv: &FileKVStore, key: &str) -> Vec<u8> {
    let mut buf = [0u8; 256];
    kv.fetch(key, &mut buf).await.unwrap().unwrap().to_vec()
}

// =========================================================================
// 6.1 Enum Load Sequence
// =========================================================================

#[tokio::test]
async fn test_enum_load_sets_variant_before_reading_fields() {
    // If variant isn't set first, field deserialization fails with Absent
    let kv = setup_kv(&[
        ("wifi/__schema_hash__", &WifiCfg::SCHEMA_HASH.to_le_bytes()),
        ("wifi/ip/_variant", b"Static"), // plain UTF-8, not postcard
        ("wifi/ip/Static/address", &encode([192u8, 168, 1, 100])),
        ("wifi/ip/Static/gateway", &encode([192u8, 168, 1, 1])),
    ])
    .await;

    let shadow = Shadow::<WifiCfg, _>::new(&kv);
    shadow.load().await.unwrap();

    let state = shadow.state().await.unwrap();
    match &state.ip {
        IpSettingsCfg::Static(cfg) => {
            assert_eq!(cfg.address, [192, 168, 1, 100]);
        }
        IpSettingsCfg::Dhcp => panic!("Wrong variant loaded"),
    }
}

#[tokio::test]
async fn test_enum_load_missing_variant_uses_default() {
    // No _variant key -> use #[default] variant
    let kv = empty_kv();

    let shadow = Shadow::<WifiCfg, _>::new(&kv);
    shadow.load().await.unwrap(); // First boot - initializes defaults

    assert!(matches!(
        shadow.state().await.unwrap().ip,
        IpSettingsCfg::Dhcp
    ));
}

#[tokio::test]
async fn test_enum_load_ignores_inactive_variant_fields() {
    // _variant says Dhcp, but Static fields exist in KV (orphans from previous)
    // Should not error, just ignore them
    let kv = setup_kv(&[
        ("wifi/__schema_hash__", &WifiCfg::SCHEMA_HASH.to_le_bytes()),
        ("wifi/ip/_variant", b"Dhcp"),
        ("wifi/ip/Static/address", &encode([192u8, 168, 1, 100])), // orphan
    ])
    .await;

    let shadow = Shadow::<WifiCfg, _>::new(&kv);
    shadow.load().await.unwrap(); // Should not fail

    assert!(matches!(
        shadow.state().await.unwrap().ip,
        IpSettingsCfg::Dhcp
    ));
}

// =========================================================================
// 6.2 Enum Persistence via First-Boot and apply_and_save
// =========================================================================

#[tokio::test]
async fn test_enum_first_boot_writes_variant_key_as_utf8() {
    // First boot (empty KV) persists defaults including _variant keys
    let kv = empty_kv();

    let shadow = Shadow::<WifiCfg, _>::new(&kv);
    let result = shadow.load().await.unwrap();

    assert!(result.first_boot);

    // Default variant (Dhcp) stored as plain UTF-8, not postcard
    let variant = kv_fetch_raw(&kv, "wifi/ip/_variant").await;
    assert_eq!(variant, b"Dhcp");
}

#[tokio::test]
async fn test_enum_apply_and_save_writes_variant_key_as_utf8() {
    // Use apply_and_save to change variant and verify UTF-8 storage
    let kv = empty_kv();

    let shadow = Shadow::<WifiCfg, _>::new(&kv);
    shadow.load().await.unwrap(); // First boot - sets Dhcp

    // Apply delta to change to Static variant
    let delta = DeltaWifiCfg {
        ip: Some(DeltaIpSettingsCfg::Static(DeltaStaticIpCfg {
            address: Some([10, 0, 0, 1]),
            gateway: Some([10, 0, 0, 254]),
        })),
    };
    shadow.apply_and_save(&delta).await.unwrap();

    // Verify variant key is plain UTF-8, not postcard
    let variant = kv_fetch_raw(&kv, "wifi/ip/_variant").await;
    assert_eq!(variant, b"Static");
}

#[tokio::test]
async fn test_enum_variant_switch_preserves_inactive_fields() {
    // Was Static, apply_and_save to Dhcp - Static fields should be PRESERVED
    // (they are valid keys, not orphans)
    let kv = setup_kv(&[
        ("wifi/__schema_hash__", &WifiCfg::SCHEMA_HASH.to_le_bytes()),
        ("wifi/ip/_variant", b"Static"),
        ("wifi/ip/Static/address", &encode([192u8, 168, 1, 100])),
        ("wifi/ip/Static/gateway", &encode([192u8, 168, 1, 1])),
    ])
    .await;

    let shadow = Shadow::<WifiCfg, _>::new(&kv);
    shadow.load().await.unwrap();

    // Verify we loaded Static variant
    match &shadow.state().await.unwrap().ip {
        IpSettingsCfg::Static(cfg) => {
            assert_eq!(cfg.address, [192, 168, 1, 100]);
        }
        _ => panic!("Expected Static variant"),
    }

    // Switch to Dhcp
    let delta = DeltaWifiCfg {
        ip: Some(DeltaIpSettingsCfg::Dhcp),
    };
    shadow.apply_and_save(&delta).await.unwrap();

    // Verify state is now Dhcp
    assert!(matches!(
        shadow.state().await.unwrap().ip,
        IpSettingsCfg::Dhcp
    ));

    // Static fields should still be in KV (preserved for future switch back)
    assert!(kv_has_key(&kv, "wifi/ip/Static/address").await);
    assert!(kv_has_key(&kv, "wifi/ip/Static/gateway").await);
}

#[tokio::test]
async fn test_enum_variant_switch_and_back_restores_values_on_reload() {
    // Design decision: Switching variants at runtime uses defaults from delta,
    // but preserved KV values are restored on RELOAD (e.g., after reboot).
    let kv = setup_kv(&[
        ("wifi/__schema_hash__", &WifiCfg::SCHEMA_HASH.to_le_bytes()),
        ("wifi/ip/_variant", b"Static"),
        ("wifi/ip/Static/address", &encode([192u8, 168, 1, 100])),
        ("wifi/ip/Static/gateway", &encode([192u8, 168, 1, 1])),
    ])
    .await;

    let shadow = Shadow::<WifiCfg, _>::new(&kv);
    shadow.load().await.unwrap();

    // Switch to Dhcp
    let delta = DeltaWifiCfg {
        ip: Some(DeltaIpSettingsCfg::Dhcp),
    };
    shadow.apply_and_save(&delta).await.unwrap();
    assert!(matches!(
        shadow.state().await.unwrap().ip,
        IpSettingsCfg::Dhcp
    ));

    // Switch back to Static (without providing address/gateway in delta)
    let delta = DeltaWifiCfg {
        ip: Some(DeltaIpSettingsCfg::Static(DeltaStaticIpCfg {
            address: None, // Not providing new values
            gateway: None,
        })),
    };
    shadow.apply_and_save(&delta).await.unwrap();

    // At runtime, we get defaults (since delta didn't provide values)
    // But on reload, we should get the preserved KV values

    // Simulate reboot by creating new shadow instance
    let shadow2 = Shadow::<WifiCfg, _>::new(&kv);
    shadow2.load().await.unwrap();

    // After reload, original values should be restored from KV
    match &shadow2.state().await.unwrap().ip {
        IpSettingsCfg::Static(cfg) => {
            assert_eq!(cfg.address, [192, 168, 1, 100]);
            assert_eq!(cfg.gateway, [192, 168, 1, 1]);
        }
        _ => panic!("Expected Static variant after reload"),
    }
}

// =========================================================================
// 6.3 Serde Rename Interaction
// =========================================================================

#[tokio::test]
async fn test_enum_serde_rename_affects_variant_key() {
    // #[serde(rename = "none")] on Open variant
    let kv = setup_kv(&[
        (
            "wifi/__schema_hash__",
            &AuthConfig::SCHEMA_HASH.to_le_bytes(),
        ),
        ("wifi/auth/_variant", b"none"), // serde-renamed
    ])
    .await;

    let shadow = Shadow::<AuthConfig, _>::new(&kv);
    shadow.load().await.unwrap();

    assert!(matches!(shadow.state().await.unwrap().auth, WifiAuth::Open));
}

// =========================================================================
// Phase 7 Tests: Delta Apply and Save
// =========================================================================

#[tokio::test]
async fn test_apply_and_save_only_persists_some_fields() {
    // Set up KV with initial values
    let kv = setup_kv(&[
        (
            "test/__schema_hash__",
            &TestConfig::SCHEMA_HASH.to_le_bytes(),
        ),
        ("test/timeout", &encode(1000u32)),
        ("test/enabled", &encode(false)),
        ("test/retries", &encode(3u8)),
    ])
    .await;

    let shadow = Shadow::<TestConfig, _>::new(&kv);
    shadow.load().await.unwrap();

    // Delta only changes timeout
    let delta = DeltaTestConfig {
        timeout: Some(5000),
        enabled: None,
        retries: None,
    };
    shadow.apply_and_save(&delta).await.unwrap();
    // delta still available if needed

    // State updated
    assert_eq!(shadow.state().await.unwrap().timeout, 5000);

    // Only timeout written to KV (check write count or value)
    // enabled and retries should have original values
    let timeout: u32 = kv_fetch_decode(&kv, "test/timeout").await;
    let enabled: bool = kv_fetch_decode(&kv, "test/enabled").await;

    assert_eq!(timeout, 5000);
    assert!(!enabled); // unchanged
}

#[tokio::test]
async fn test_apply_and_save_enum_variant_change() {
    // Delta changes enum variant from Dhcp to Static
    let kv = setup_kv(&[
        ("wifi/__schema_hash__", &WifiCfg::SCHEMA_HASH.to_le_bytes()),
        ("wifi/ip/_variant", b"Dhcp"),
    ])
    .await;

    let shadow = Shadow::<WifiCfg, _>::new(&kv);
    shadow.load().await.unwrap();
    assert!(matches!(
        shadow.state().await.unwrap().ip,
        IpSettingsCfg::Dhcp
    ));

    // Apply delta that changes to Static variant with values
    let delta = DeltaWifiCfg {
        ip: Some(DeltaIpSettingsCfg::Static(DeltaStaticIpCfg {
            address: Some([10, 0, 0, 50]),
            gateway: Some([10, 0, 0, 1]),
        })),
    };
    shadow.apply_and_save(&delta).await.unwrap();

    // Verify state changed
    match &shadow.state().await.unwrap().ip {
        IpSettingsCfg::Static(cfg) => {
            assert_eq!(cfg.address, [10, 0, 0, 50]);
            assert_eq!(cfg.gateway, [10, 0, 0, 1]);
        }
        _ => panic!("Expected Static variant"),
    }

    // Verify KV was updated
    let variant = kv_fetch_raw(&kv, "wifi/ip/_variant").await;
    assert_eq!(variant, b"Static");

    let addr: [u8; 4] = kv_fetch_decode(&kv, "wifi/ip/Static/address").await;
    assert_eq!(addr, [10, 0, 0, 50]);
}

#[tokio::test]
async fn test_apply_and_save_nested_struct_fields() {
    let kv = setup_kv(&[
        (
            "device/__schema_hash__",
            &DeviceShadow::SCHEMA_HASH.to_le_bytes(),
        ),
        ("device/config/timeout", &encode(1000u32)),
        ("device/config/retries", &encode(3u8)),
        ("device/version", &encode(1u32)),
    ])
    .await;

    let shadow = Shadow::<DeviceShadow, _>::new(&kv);
    shadow.load().await.unwrap();

    // Delta changes only nested timeout
    let delta = DeltaDeviceShadow {
        config: Some(DeltaInnerConfig {
            timeout: Some(9999),
            retries: None,
        }),
        version: None,
    };
    shadow.apply_and_save(&delta).await.unwrap();

    // Only timeout updated
    let timeout: u32 = kv_fetch_decode(&kv, "device/config/timeout").await;
    let retries: u8 = kv_fetch_decode(&kv, "device/config/retries").await;

    assert_eq!(timeout, 9999);
    assert_eq!(retries, 3); // unchanged
}

// =========================================================================
// Phase 8 Tests: Adjacently-Tagged Enum Support
// =========================================================================

mod adjacently_tagged {
    use super::*;
    use rustot_derive::shadow_node;

    // Inner config type for Sio variant
    #[shadow_node]
    #[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
    pub struct SioConfig {
        #[serde(rename = "polarity")]
        pub polarity: bool,
    }

    // Inner config type for IoLink variant
    #[shadow_node]
    #[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
    pub struct IoLinkConfig {
        pub cycle_time: u16,
    }

    // Adjacently-tagged enum
    #[shadow_node]
    #[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
    #[serde(tag = "mode", content = "config", rename_all = "lowercase")]
    pub enum PortMode {
        #[default]
        Inactive,
        Sio(SioConfig),
        IoLink(IoLinkConfig),
    }

    #[test]
    fn test_adjacently_tagged_delta_is_struct() {
        // Verify DeltaPortMode is a struct with mode and config fields
        let delta = DeltaPortMode {
            mode: Some(PortModeVariant::Sio),
            config: Some(DeltaPortModeConfig::Sio(DeltaSioConfig {
                polarity: Some(true),
            })),
        };

        // Test Default
        let default_delta = DeltaPortMode::default();
        assert!(default_delta.mode.is_none());
        assert!(default_delta.config.is_none());

        // Test mode-only
        let mode_only = DeltaPortMode {
            mode: Some(PortModeVariant::Inactive),
            config: None,
        };
        assert_eq!(mode_only.mode, Some(PortModeVariant::Inactive));

        // Test config-only
        let config_only = DeltaPortMode {
            mode: None,
            config: Some(DeltaPortModeConfig::IoLink(DeltaIoLinkConfig {
                cycle_time: Some(1000),
            })),
        };
        assert!(config_only.mode.is_none());

        // Verify that the delta has both mode and config
        assert_eq!(delta.mode, Some(PortModeVariant::Sio));
    }

    #[test]
    fn test_adjacently_tagged_variant_enum_generated() {
        // Verify PortModeVariant enum exists and has correct variants
        let inactive = PortModeVariant::Inactive;
        let sio = PortModeVariant::Sio;
        let iolink = PortModeVariant::IoLink;

        // Test equality
        assert_eq!(inactive, PortModeVariant::Inactive);
        assert_ne!(inactive, sio);
        assert_ne!(sio, iolink);

        // Test Default (should be Inactive)
        let default_variant = PortModeVariant::default();
        assert_eq!(default_variant, PortModeVariant::Inactive);

        // Test Clone and Copy
        let cloned = inactive.clone();
        assert_eq!(cloned, inactive);

        let copied: PortModeVariant = inactive;
        assert_eq!(copied, inactive);
    }

    #[test]
    fn test_adjacently_tagged_delta_serialization() {
        // Test JSON serialization of delta with mode and config
        let delta = DeltaPortMode {
            mode: Some(PortModeVariant::Sio),
            config: Some(DeltaPortModeConfig::Sio(DeltaSioConfig {
                polarity: Some(true),
            })),
        };

        let json = serde_json::to_string(&delta).unwrap();
        // Should have "mode" and "config" keys (renamed from fields)
        assert!(json.contains("\"mode\""));
        assert!(json.contains("\"config\""));
        assert!(json.contains("\"sio\"")); // lowercase due to rename_all
        assert!(json.contains("\"polarity\""));

        // Test deserialization
        let parsed: DeltaPortMode = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.mode, Some(PortModeVariant::Sio));
    }

    #[test]
    fn test_adjacently_tagged_mode_only_delta() {
        // Test mode-only delta (no config)
        let json = r#"{"mode": "inactive"}"#;
        let delta: DeltaPortMode = serde_json::from_str(json).unwrap();
        assert_eq!(delta.mode, Some(PortModeVariant::Inactive));
        assert!(delta.config.is_none());
    }

    #[test]
    fn test_adjacently_tagged_config_only_delta() {
        // Test config-only delta (partial update)
        let json = r#"{"config": {"sio": {"polarity": true}}}"#;
        let delta: DeltaPortMode = serde_json::from_str(json).unwrap();
        assert!(delta.mode.is_none());
        assert!(delta.config.is_some());

        if let Some(DeltaPortModeConfig::Sio(sio_config)) = delta.config {
            assert_eq!(sio_config.polarity, Some(true));
        } else {
            panic!("Expected Sio config");
        }
    }

    #[test]
    fn test_adjacently_tagged_reported_type() {
        // Test that Reported type is an enum
        let _inactive = ReportedPortMode::Inactive;
        let sio = ReportedPortMode::Sio(ReportedSioConfig {
            polarity: Some(false),
        });

        // Verify Default works (should be Inactive)
        let default_reported = ReportedPortMode::default();
        match default_reported {
            ReportedPortMode::Inactive => {}
            _ => panic!("Expected Inactive as default"),
        }

        // Verify Clone works
        let cloned_sio = sio.clone();
        match cloned_sio {
            ReportedPortMode::Sio(config) => {
                assert_eq!(config.polarity, Some(false));
            }
            _ => panic!("Expected Sio variant"),
        }
    }

    #[test]
    fn test_adjacently_tagged_into_reported() {
        // Test into_reported() conversion
        let port_mode = PortMode::Sio(SioConfig { polarity: true });
        let reported = port_mode.into_reported();

        match reported {
            ReportedPortMode::Sio(config) => {
                assert_eq!(config.polarity, Some(true));
            }
            _ => panic!("Expected Sio variant"),
        }

        // Test Inactive variant
        let inactive = PortMode::Inactive;
        let reported_inactive = inactive.into_reported();
        match reported_inactive {
            ReportedPortMode::Inactive => {}
            _ => panic!("Expected Inactive variant"),
        }
    }

    #[test]
    fn test_adjacently_tagged_reported_serialization() {
        // Test flat union serialization of Reported type
        let reported = ReportedPortMode::Sio(ReportedSioConfig {
            polarity: Some(true),
        });

        let json = serde_json::to_string(&reported).unwrap();
        // Should serialize as a flat object with mode field
        assert!(json.contains("\"mode\""));
        assert!(json.contains("\"sio\""));
        assert!(json.contains("\"polarity\""));
    }

    #[test]
    fn test_adjacently_tagged_apply_mode_only() {
        use crate::shadows::ShadowNode;
        // Test applying a mode-only delta
        let mut port_mode = PortMode::Inactive;

        let delta = DeltaPortMode {
            mode: Some(PortModeVariant::Sio),
            config: None,
        };

        port_mode.apply_delta(&delta);

        // Variant should change to Sio with default config
        match port_mode {
            PortMode::Sio(config) => {
                assert_eq!(config.polarity, false); // Default
            }
            _ => panic!("Expected Sio variant"),
        }
    }

    #[test]
    fn test_adjacently_tagged_apply_config_only_matching_variant() {
        use crate::shadows::ShadowNode;
        // Test applying config-only delta when variant matches
        let mut port_mode = PortMode::Sio(SioConfig { polarity: false });

        let delta = DeltaPortMode {
            mode: None,
            config: Some(DeltaPortModeConfig::Sio(DeltaSioConfig {
                polarity: Some(true),
            })),
        };

        port_mode.apply_delta(&delta);

        // Config should update
        match port_mode {
            PortMode::Sio(config) => {
                assert_eq!(config.polarity, true);
            }
            _ => panic!("Expected Sio variant"),
        }
    }

    #[test]
    fn test_adjacently_tagged_apply_config_only_wrong_variant() {
        use crate::shadows::ShadowNode;
        // Test applying config-only delta when variant doesn't match
        let mut port_mode = PortMode::Inactive;

        let delta = DeltaPortMode {
            mode: None,
            config: Some(DeltaPortModeConfig::Sio(DeltaSioConfig {
                polarity: Some(true),
            })),
        };

        port_mode.apply_delta(&delta);

        // With new design, mismatched config is ignored (no error)
        // Variant should remain Inactive
        assert!(matches!(port_mode, PortMode::Inactive));
    }

    #[test]
    fn test_adjacently_tagged_apply_mode_and_config() {
        use crate::shadows::ShadowNode;
        // Test applying both mode and config in one delta
        let mut port_mode = PortMode::Inactive;

        let delta = DeltaPortMode {
            mode: Some(PortModeVariant::IoLink),
            config: Some(DeltaPortModeConfig::IoLink(DeltaIoLinkConfig {
                cycle_time: Some(5000),
            })),
        };

        port_mode.apply_delta(&delta);

        // Variant should change and config should update
        match port_mode {
            PortMode::IoLink(config) => {
                assert_eq!(config.cycle_time, 5000);
            }
            _ => panic!("Expected IoLink variant"),
        }
    }
}
