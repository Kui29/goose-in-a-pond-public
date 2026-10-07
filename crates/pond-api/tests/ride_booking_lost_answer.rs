//! The ride company's answer to a confirmed booking is lost: the phone is told the outcome is
//! not known, never that the ride failed, and the ride stays the member's to check.

#[path = "support/ride_phone.rs"]
mod ride_phone;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use pond_core::rides::booking::RideBooking;
use pond_core::rides::mocks::{MockRideProvider, OnCurrent, OnRequest};
use ride_phone::{member_with_phone, send, trip};

#[tokio::test]
async fn a_lost_answer_is_accepted_not_failed_and_not_requested_again() {
    let provider = Arc::new(MockRideProvider::new().on_request(OnRequest::LoseTheAnswer));
    provider.set_current(OnCurrent::Unreachable);
    pond_api::rides::install(Arc::new(RideBooking::new(provider.clone())));
    let p = ride_phone::pond().await;
    let phone = member_with_phone(&p, "liz").await;

    let (_, quoted) = send(
        &p.lan,
        Method::POST,
        "/api/v1/rides/quote",
        Some(&phone),
        Some(trip()),
    )
    .await;
    let id = quoted["id"].as_str().unwrap();
    let confirm = format!("/api/v1/rides/{id}/confirm");

    let (status, body) = send(&p.lan, Method::POST, &confirm, Some(&phone), None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["state"]["state"], "outcome_unknown");
    assert!(
        body["message"]
            .as_str()
            .unwrap_or("")
            .contains("check its app"),
        "{body}"
    );

    let (status, _) = send(&p.lan, Method::POST, &confirm, Some(&phone), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(provider.requests(), 1, "a lost answer was requested again");
}
