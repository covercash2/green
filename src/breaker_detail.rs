use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::io::load_toml_file;

pub trait BreakerDetailStore: Send + Sync + std::fmt::Debug {
    fn get(&self, key: &str) -> Option<&BreakerSlot>;
    fn row_count(&self) -> u32;
    fn todos(&self) -> &[String];
    fn coupled_primary_of(&self, key: &str) -> Option<&str>;
    fn coupled_secondary_of(&self, key: &str) -> Option<&str>;
    fn is_coupled_primary(&self, key: &str) -> bool;
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BreakerSlot {
    pub label: Option<String>,
    pub amperage: Option<String>,
    pub devices: Option<Vec<String>>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CoupledPair {
    pub primary: String,
    pub secondary: String,
}

#[derive(Debug, Deserialize)]
pub struct BreakerData {
    #[serde(default)]
    pub todos: Vec<String>,
    pub slots: HashMap<String, BreakerSlot>,
    #[serde(default)]
    pub couples: Vec<CoupledPair>,
}

impl BreakerData {
    pub async fn load() -> Result<Self, BreakerStoreError> {
        let data = load_toml_file("assets/breaker_details.toml").await?;

        Ok(data)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BreakerStoreError {
    #[error("failed to read breaker data file: {0}")]
    Io(#[from] crate::io::IoError),

    #[error("coupled pair `{primary}` and `{secondary}` are on different sides")]
    MismatchedSides { primary: String, secondary: String },

    #[error("slot `{key}` appears in multiple coupling declarations")]
    DuplicateCoupling { key: String },
}

#[derive(Debug)]
pub struct BreakerStore {
    slots: HashMap<String, BreakerSlot>,
    row_count: u32,
    todos: Vec<String>,
    /// Maps secondary key → primary key
    coupled_secondaries: HashMap<String, String>,
    /// Maps primary key → secondary key
    coupled_primaries: HashMap<String, String>,
}

impl BreakerStore {
    pub fn from_data(data: BreakerData) -> Result<Self, BreakerStoreError> {
        let row_count = data
            .slots
            .keys()
            .filter_map(|k| k.split_once('-')?.0.parse::<u32>().ok())
            .max()
            .unwrap_or(0);

        let mut coupled_secondaries: HashMap<String, String> = HashMap::new();
        let mut coupled_primaries: HashMap<String, String> = HashMap::new();
        let mut seen_in_couples: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for pair in &data.couples {
            let primary_side = pair.primary.split_once('-').map(|(_, s)| s);
            let secondary_side = pair.secondary.split_once('-').map(|(_, s)| s);
            if primary_side != secondary_side {
                return Err(BreakerStoreError::MismatchedSides {
                    primary: pair.primary.clone(),
                    secondary: pair.secondary.clone(),
                });
            }

            if !seen_in_couples.insert(pair.primary.clone()) {
                return Err(BreakerStoreError::DuplicateCoupling {
                    key: pair.primary.clone(),
                });
            }
            if !seen_in_couples.insert(pair.secondary.clone()) {
                return Err(BreakerStoreError::DuplicateCoupling {
                    key: pair.secondary.clone(),
                });
            }

            coupled_primaries.insert(pair.primary.clone(), pair.secondary.clone());
            coupled_secondaries.insert(pair.secondary.clone(), pair.primary.clone());
        }

        Ok(Self {
            slots: data.slots,
            row_count,
            todos: data.todos,
            coupled_secondaries,
            coupled_primaries,
        })
    }
}

impl BreakerDetailStore for BreakerStore {
    fn get(&self, key: &str) -> Option<&BreakerSlot> {
        self.slots.get(key)
    }

    fn row_count(&self) -> u32 {
        self.row_count
    }

    fn todos(&self) -> &[String] {
        &self.todos
    }

    fn coupled_primary_of(&self, key: &str) -> Option<&str> {
        self.coupled_secondaries.get(key).map(|s| s.as_str())
    }

    fn coupled_secondary_of(&self, key: &str) -> Option<&str> {
        self.coupled_primaries.get(key).map(|s| s.as_str())
    }

    fn is_coupled_primary(&self, key: &str) -> bool {
        self.coupled_primaries.contains_key(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_store() -> BreakerStore {
        let data = BreakerData {
            todos: vec!["master bedroom: breaker not yet identified".into()],
            slots: [
                (
                    "1-left".into(),
                    BreakerSlot {
                        label: Some("garage".into()),
                        amperage: Some("20A".into()),
                        devices: Some(vec![
                            "garage outlet west wall".into(),
                            "garage door opener (ceiling)".into(),
                        ]),
                        notes: Some("AFCI protected".into()),
                    },
                ),
                (
                    "7-right".into(),
                    BreakerSlot {
                        label: Some("upstairs and living room ceiling lights".into()),
                        amperage: Some("15A".into()),
                        devices: Some(vec![
                            "west bedroom ceiling fixture".into(),
                            "south bedroom ceiling fixture".into(),
                        ]),
                        notes: None,
                    },
                ),
            ]
            .into(),
            couples: vec![],
        };
        BreakerStore::from_data(data).unwrap()
    }

    #[test]
    fn test_get_known_key() {
        let store = fixture_store();
        let slot = store.get("1-left").unwrap();
        assert_eq!(slot.label.as_deref(), Some("garage"));
        assert_eq!(slot.amperage.as_deref(), Some("20A"));
        assert_eq!(slot.devices.as_ref().unwrap().len(), 2);
        assert_eq!(slot.notes.as_deref(), Some("AFCI protected"));
    }

    #[test]
    fn test_get_unknown_key() {
        let store = fixture_store();
        assert!(store.get("99-left").is_none());
    }

    #[test]
    fn test_empty_store() {
        let store = BreakerStore::from_data(BreakerData {
            todos: vec![],
            slots: HashMap::new(),
            couples: vec![],
        })
        .unwrap();
        assert!(store.get("1-left").is_none());
        assert_eq!(store.row_count(), 0);
    }

    #[test]
    fn test_row_count_derived_from_max_key() {
        let store = fixture_store();
        assert_eq!(store.row_count(), 7);
    }

    #[test]
    fn test_todos() {
        let store = fixture_store();
        assert_eq!(store.todos().len(), 1);
        assert!(store.todos()[0].contains("master bedroom"));
    }

    #[test]
    fn test_all_fields_optional() {
        let store = BreakerStore::from_data(BreakerData {
            todos: vec![],
            slots: [(
                "1-left".into(),
                BreakerSlot {
                    label: None,
                    amperage: None,
                    devices: None,
                    notes: None,
                },
            )]
            .into(),
            couples: vec![],
        })
        .unwrap();
        let slot = store.get("1-left").unwrap();
        assert!(slot.label.is_none());
        assert!(slot.amperage.is_none());
        assert!(slot.devices.is_none());
        assert!(slot.notes.is_none());
    }

    #[test]
    fn test_coupled_primary_of() {
        let data = BreakerData {
            todos: vec![],
            slots: HashMap::new(),
            couples: vec![CoupledPair {
                primary: "1-right".into(),
                secondary: "2-right".into(),
            }],
        };
        let store = BreakerStore::from_data(data).unwrap();
        assert_eq!(store.coupled_primary_of("2-right"), Some("1-right"));
        assert_eq!(store.coupled_primary_of("1-right"), None);
        assert!(store.is_coupled_primary("1-right"));
        assert!(!store.is_coupled_primary("2-right"));
    }

    #[test]
    fn test_from_data_error_mismatched_sides() {
        let data = BreakerData {
            todos: vec![],
            slots: HashMap::new(),
            couples: vec![CoupledPair {
                primary: "1-right".into(),
                secondary: "2-left".into(),
            }],
        };
        let err = BreakerStore::from_data(data).unwrap_err();
        assert!(matches!(err, BreakerStoreError::MismatchedSides { .. }));
    }

    #[test]
    fn test_from_data_error_duplicate_coupling() {
        let data = BreakerData {
            todos: vec![],
            slots: HashMap::new(),
            couples: vec![
                CoupledPair {
                    primary: "1-right".into(),
                    secondary: "2-right".into(),
                },
                CoupledPair {
                    primary: "3-right".into(),
                    secondary: "2-right".into(),
                },
            ],
        };
        let err = BreakerStore::from_data(data).unwrap_err();
        assert!(matches!(err, BreakerStoreError::DuplicateCoupling { .. }));
    }
}
