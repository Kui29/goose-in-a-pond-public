//! Test double for [`MemberNotifier`]: each member owns the devices it is given, and every
//! delivery is recorded.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::mcp::ports::notification::{MemberDelivery, MemberNotifier, Notification};

#[derive(Default)]
pub struct MockMemberNotifier {
    devices: HashMap<String, Vec<String>>,
    sent: Mutex<Vec<(String, Notification)>>,
    /// How many deliveries from now on fail, as an unreadable device list would.
    failures_left: Mutex<usize>,
}

impl MockMemberNotifier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Give `profile_id` these devices; a member never given any has no phone.
    pub fn with_devices(mut self, profile_id: &str, devices: &[&str]) -> Self {
        self.devices.insert(
            profile_id.to_string(),
            devices.iter().map(|d| d.to_string()).collect(),
        );
        self
    }

    /// The next `n` deliveries fail, whoever they are for.
    pub fn failing_next(self, n: usize) -> Self {
        *self.failures_left.lock().unwrap() = n;
        self
    }

    /// Every `(profile_id, notification)` asked for, in order, delivered or not.
    pub fn sent(&self) -> Vec<(String, Notification)> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl MemberNotifier for MockMemberNotifier {
    async fn notify_member(&self, profile_id: &str, notification: Notification) -> MemberDelivery {
        self.sent
            .lock()
            .unwrap()
            .push((profile_id.to_string(), notification));
        {
            let mut left = self.failures_left.lock().unwrap();
            if *left > 0 {
                *left -= 1;
                return MemberDelivery::Failed("the device list could not be read".to_string());
            }
        }
        match self.devices.get(profile_id) {
            Some(devices) if !devices.is_empty() => MemberDelivery::Reached(devices.clone()),
            _ => MemberDelivery::NoPhone,
        }
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
    async fn a_member_with_devices_is_reached_and_one_without_has_no_phone() {
        let notifier = MockMemberNotifier::new().with_devices("liz", &["liz-phone"]);
        assert_eq!(
            notifier.notify_member("liz", notification()).await,
            MemberDelivery::Reached(vec!["liz-phone".to_string()])
        );
        assert_eq!(
            notifier.notify_member("jerry", notification()).await,
            MemberDelivery::NoPhone
        );
        assert_eq!(notifier.sent().len(), 2);
    }

    #[tokio::test]
    async fn a_scripted_failure_is_a_failure_and_then_delivery_resumes() {
        let notifier = MockMemberNotifier::new()
            .with_devices("liz", &["liz-phone"])
            .failing_next(1);
        assert!(matches!(
            notifier.notify_member("liz", notification()).await,
            MemberDelivery::Failed(_)
        ));
        assert!(matches!(
            notifier.notify_member("liz", notification()).await,
            MemberDelivery::Reached(_)
        ));
    }
}
