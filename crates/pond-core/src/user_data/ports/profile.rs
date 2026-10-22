//! Profile port — driven port for household member profile persistence.

use crate::user_data::domain::profile::{CreateProfileRequest, HouseholdRole, Profile};
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;

#[async_trait]
pub trait ProfileRepository: Send + Sync {
    /// The first profile becomes the owner; later ones are members.
    async fn create(&self, request: CreateProfileRequest) -> Result<Profile>;
    async fn get(&self, profile_id: &str) -> Result<Option<Profile>>;
    async fn list(&self) -> Result<Vec<Profile>>;
    async fn update_preferences(
        &self,
        profile_id: &str,
        prefs: HashMap<String, String>,
    ) -> Result<Profile>;
    async fn set_role(&self, profile_id: &str, role: HouseholdRole) -> Result<Profile>;
    async fn delete(&self, profile_id: &str) -> Result<()>;
}
