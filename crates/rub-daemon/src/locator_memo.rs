use std::collections::{HashMap, VecDeque};

const LOCATOR_MEMO_LIMIT: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LocatorMemoTarget {
    ElementRef(String),
    Index(u32),
}

#[derive(Debug, Default)]
pub struct LocatorMemoRegistry {
    entries: HashMap<String, Vec<LocatorMemoTarget>>,
    order: VecDeque<String>,
}

impl LocatorMemoRegistry {
    pub fn get(&self, key: &str) -> Option<Vec<LocatorMemoTarget>> {
        self.entries.get(key).cloned()
    }

    pub fn insert(&mut self, key: String, targets: Vec<LocatorMemoTarget>) {
        if targets.is_empty() {
            return;
        }

        self.entries.insert(key.clone(), targets);
        self.order.retain(|existing| existing != &key);
        self.order.push_back(key);

        while self.order.len() > LOCATOR_MEMO_LIMIT {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LocatorMemoRegistry, LocatorMemoTarget};

    #[test]
    fn locator_memo_registry_is_bounded_and_refreshes_order() {
        let mut registry = LocatorMemoRegistry::default();
        for index in 0..130 {
            registry.insert(
                format!("key-{index}"),
                vec![LocatorMemoTarget::Index(index as u32)],
            );
        }

        assert!(registry.get("key-0").is_none());
        assert_eq!(
            registry.get("key-129").unwrap(),
            vec![LocatorMemoTarget::Index(129)]
        );

        registry.insert(
            "key-10".to_string(),
            vec![LocatorMemoTarget::ElementRef("main:10".into())],
        );
        assert_eq!(
            registry.get("key-10").unwrap(),
            vec![LocatorMemoTarget::ElementRef("main:10".into())]
        );
    }
}
