//! Test double for [`MemberNotifier`]: each member owns the devices it is given, and every
//! delivery is recorded.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::mcp::ports::notification::{MemberNotifier, Notification};

#[derive(Default)]
pub struct MockMemberNotifier {
    devices: HashMap<String, Vec<String>>,
    sent: Mutex<Vec<(String, Notification)>>,
}

impl MockMemberNotifier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Give `profile_id` these devices; a member never given any reaches nobody.
    pub fn with_devices(mut self, profile_id: &str, devices: &[&str]) -> Self {
        self.devices.insert(
            profile_id.to_string(),
            devices.iter().map(|d| d.to_string()).collect(),
        );
        self
    }

    /// Every `(profile_id, notification)` asked for, in order, delivered or not.
    pub fn sent(&self) -> Vec<(String, Notification)> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl MemberNotifier for MockMemberNotifier {
    async fn notify_member(&self, profile_id: &str, notification: Notification) -> Vec<String> {
        self.sent
            .lock()
            .unwrap()
            .push((profile_id.to_string(), notification));
        self.devices.get(profile_id).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notification() -> Notification {
        Notification {
            id: "n-1".to_string(),
            target: "broadcast".to_string(),
            category: "info".to_string(),
            title: "t".to_string(),
            body: "b".to_string(),
            timestamp: "2026-10-06T00:00:00Z".to_string(),
            data: None,
        }
    }

    #[tokio::test]
    async fn a_member_with_devices_is_reached_and_one_without_is_not() {
        let notifier = MockMemberNotifier::new().with_devices("liz", &["liz-phone"]);
        assert_eq!(
            notifier.notify_member("liz", notification()).await,
            vec!["liz-phone".to_string()]
        );
        assert!(notifier
            .notify_member("jerry", notification())
            .await
            .is_empty());
        assert_eq!(notifier.sent().len(), 2);
    }
}
