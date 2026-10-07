//! Driven port: push events from GIAP to connected clients (e.g. the GOTG mobile app).
//!
//! TODO: categories, acknowledgement, history and persistence, a WebSocket or SSE transport.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A notification to push to connected devices.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    /// Target device ID, or "broadcast" for all
    pub target: String,
    /// "alert", "info", "action_required"
    pub category: String,
    pub title: String,
    pub body: String,
    pub timestamp: String,
    /// Arbitrary payload for the client to act on
    pub data: Option<serde_json::Value>,
}

/// Driven Port: push notifications to connected clients.
#[async_trait]
pub trait NotificationSender: Send + Sync {
    /// Send a notification to a specific device or broadcast.
    async fn send(&self, notification: Notification) -> Result<()>;

    async fn broadcast(&self, notification: Notification) -> Result<()>;
}

/// What became of a notification for one household member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberDelivery {
    /// Queued for these devices of theirs; never empty.
    Reached(Vec<String>),
    /// The member has no paired device of their own.
    NoPhone,
    /// Nothing was delivered: their devices could not be read, the id is not a member, or every
    /// write failed. The reason is for logs.
    Failed(String),
}

/// Driven Port: deliver to one household member's own devices, never a broadcast.
#[async_trait]
pub trait MemberNotifier: Send + Sync {
    async fn notify_member(&self, profile_id: &str, notification: Notification) -> MemberDelivery;
}
