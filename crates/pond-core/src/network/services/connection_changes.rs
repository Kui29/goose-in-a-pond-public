//! Reading the pond's network links takes any paired token; changing them can cut every phone off
//! the pond, so it takes a household role (`household_access::Action::ChangeConnections`).

/// The wire code every refused change answers with.
pub const NOT_ALLOWED: &str = "not_allowed";
