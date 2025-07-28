use core::hash::Hash;

use serde::{de::DeserializeOwned, Serialize};

use crate::shadows::ShadowPatch;

impl ShadowPatch for std::string::String {
    type Delta = Self;

    type Reported = Self;

    fn apply_patch(&mut self, delta: Self::Delta) {
        *self = delta;
    }

    fn into_reported(self) -> Self::Reported {
        self
    }
}

impl<T: Clone + Serialize + DeserializeOwned> ShadowPatch for std::vec::Vec<T> {
    type Delta = Self;

    type Reported = Self;

    fn apply_patch(&mut self, delta: Self::Delta) {
        *self = delta;
    }

    fn into_reported(self) -> Self::Reported {
        self
    }
}

impl<K, V> ShadowPatch for std::collections::HashMap<K, V>
where
    K: Clone + Serialize + DeserializeOwned + Eq + Hash,
    V: ShadowPatch,
{
    type Delta = std::collections::HashMap<K, <V as ShadowPatch>::Delta>;

    type Reported = std::collections::HashMap<K, <V as ShadowPatch>::Reported>;

    fn apply_patch(&mut self, delta: Self::Delta) {
        for (key, value) in delta.into_iter() {
            if let Some(entry) = self.get_mut(&key) {
                entry.apply_patch(value.clone());
            } else {
                let mut entry = V::default();
                entry.apply_patch(value.clone());
                self.insert(key.clone(), entry);
            }
        }
    }

    fn into_reported(self) -> Self::Reported {
        self.into_iter()
            .map(|(k, v)| (k, v.into_reported()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate as rustot;
    use crate::shadows::ShadowPatch;
    use rustot_derive::shadow_patch;
    use serde::{Deserialize, Serialize};

    use std::collections::HashMap;

    #[test]
    fn string_shadow_patch() {
        let mut s = String::from("hello");

        // Test apply_patch with Some
        s.apply_patch(String::from("world"));
        assert_eq!(s, "world");

        // Test into_reported
        assert_eq!(s.into_reported(), "world");
    }

    #[test]
    fn vec_shadow_patch() {
        let mut v = vec![1, 2, 3];

        // Test apply_patch with Some
        v.apply_patch(vec![4, 5]);
        assert_eq!(v, vec![4, 5]);

        // Test into_reported
        assert_eq!(v.into_reported(), vec![4, 5]);
    }

    #[test]
    fn hashmap_shadow_patch() {
        let mut map = HashMap::new();
        map.insert("a".to_string(), String::from("alpha"));
        map.insert("b".to_string(), String::from("beta"));

        let mut delta = HashMap::new();
        delta.insert("a".to_string(), String::from("updated"));
        delta.insert("c".to_string(), String::from("gamma"));

        // Test apply_patch
        map.apply_patch(delta);
        assert_eq!(map.get("a"), Some(&String::from("updated")));
        assert_eq!(map.get("b"), Some(&String::from("beta")));
        assert_eq!(map.get("c"), Some(&String::from("gamma")));

        // Test into_reported
        let reported = map.into_reported();
        assert_eq!(reported.get("a"), Some(&String::from("updated")));
        assert_eq!(reported.get("b"), Some(&String::from("beta")));
        assert_eq!(reported.get("c"), Some(&String::from("gamma")));
    }

    #[shadow_patch]
    #[derive(Default, Clone, Debug, PartialEq, Deserialize, Serialize)]
    struct Device {
        name: String,
        temperature: f32,
    }

    #[test]
    fn hashmap_with_struct_values() {
        let mut devices = HashMap::new();
        devices.insert(
            "sensor1".to_string(),
            Device {
                name: "Temperature Sensor".to_string(),
                temperature: 22.5,
            },
        );
        devices.insert(
            "sensor2".to_string(),
            Device {
                name: "Humidity Sensor".to_string(),
                temperature: 18.0,
            },
        );

        let mut delta = HashMap::new();
        delta.insert(
            "sensor1".to_string(),
            DeltaDevice {
                name: Some("Temperature Sensor".to_string()),
                temperature: Some(25.0),
            },
        );
        delta.insert(
            "sensor3".to_string(),
            DeltaDevice {
                name: Some("New Sensor".to_string()),
                temperature: Some(20.0),
            },
        );

        // Apply patch
        devices.apply_patch(delta);

        // Verify updates
        assert_eq!(
            devices.get("sensor1"),
            Some(&Device {
                name: "Temperature Sensor".to_string(),
                temperature: 25.0,
            })
        );
        assert_eq!(
            devices.get("sensor2"),
            Some(&Device {
                name: "Humidity Sensor".to_string(),
                temperature: 18.0,
            })
        );
        assert_eq!(
            devices.get("sensor3"),
            Some(&Device {
                name: "New Sensor".to_string(),
                temperature: 20.0,
            })
        );

        // Test into_reported
        let reported = devices.into_reported();
        assert_eq!(reported.len(), 3);
        assert_eq!(
            reported.get("sensor1"),
            Some(&ReportedDevice {
                name: Some("Temperature Sensor".to_string()),
                temperature: Some(25.0),
            })
        );
    }
}
