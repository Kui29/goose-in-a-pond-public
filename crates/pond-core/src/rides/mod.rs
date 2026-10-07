//! Ordering and tracking rides for one household member: a fare quote, the member's own
//! confirmation, the request, and the trip's status. Provider-neutral; adapters implement
//! [`ports::RideProvider`].

pub mod booking;
pub mod domain;
pub mod ports;
pub mod tracking;

#[cfg(any(test, feature = "test-mocks"))]
pub mod mocks;
