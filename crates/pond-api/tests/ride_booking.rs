//! Booking a ride from a member's own paired phone, against a stand-in ride company: who the
//! member is, the fare-then-confirm order, and that one member never touches another's ride.

#[path = "support/ride_phone.rs"]
mod ride_phone;

use std::sync::{Arc, LazyLock};

use axum::http::{Method, StatusCode};
use pond_core::rides::booking::RideBooking;
use pond_core::rides::mocks::MockRideProvider;
use ride_phone::{member_and_phone, member_with_phone, phone_of_nobody, send, trip, Pond};
use serde_json::json;

/// One stand-in company for the whole binary: `rides::install` is process-wide.
static PROVIDER: LazyLock<Arc<MockRideProvider>> = LazyLock::new(|| {
    let provider = Arc::new(MockRideProvider::new());
    pond_api::rides::install(Arc::new(RideBooking::new(provider.clone())));
    provider
});

async fn pond() -> Pond {
    LazyLock::force(&PROVIDER);
    ride_phone::pond().await
}

#[tokio::test]
async fn a_member_gets_a_fare_confirms_and_cancels_from_their_phone() {
    let p = pond().await;
    // Counted for this member alone: the other tests here book through the same stand-in at the
    // same time, so a count across everyone races them.
    let (liz, phone) = member_and_phone(&p, "liz").await;

    let (status, quoted) = send(
        &p.lan,
        Method::POST,
        "/api/v1/rides/quote",
        Some(&phone),
        Some(trip()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{quoted}");
    assert_eq!(quoted["fare"]["display"], "KES 1,250");
    assert_eq!(quoted["pickup"]["name"], "Pickup");
    assert_eq!(quoted["state"]["state"], "awaiting_confirmation");
    assert_eq!(PROVIDER.requests_for(&liz), 0, "a quote booked a ride");
    let id = quoted["id"].as_str().unwrap();

    let (status, confirmed) = send(
        &p.lan,
        Method::POST,
        &format!("/api/v1/rides/{id}/confirm"),
        Some(&phone),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{confirmed}");
    assert_eq!(confirmed["state"]["state"], "requested");
    assert_eq!(PROVIDER.requests_for(&liz), 1);

    let (status, _) = send(
        &p.lan,
        Method::POST,
        &format!("/api/v1/rides/{id}/confirm"),
        Some(&phone),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a second confirm was accepted"
    );
    assert_eq!(PROVIDER.requests_for(&liz), 1);

    let (status, _) = send(
        &p.lan,
        Method::POST,
        &format!("/api/v1/rides/{id}/cancel"),
        Some(&phone),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn another_members_phone_cannot_see_confirm_or_cancel_the_ride() {
    let p = pond().await;
    let liz = member_with_phone(&p, "liz2").await;
    let jerry = member_with_phone(&p, "jerry").await;
    let (_, quoted) = send(
        &p.lan,
        Method::POST,
        "/api/v1/rides/quote",
        Some(&liz),
        Some(trip()),
    )
    .await;
    let id = quoted["id"].as_str().unwrap();

    for (method, path) in [
        (Method::GET, format!("/api/v1/rides/{id}")),
        (Method::POST, format!("/api/v1/rides/{id}/confirm")),
        (Method::POST, format!("/api/v1/rides/{id}/decline")),
        (Method::POST, format!("/api/v1/rides/{id}/cancel")),
    ] {
        let (status, _) = send(&p.lan, method, &path, Some(&jerry), None).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{path} answered another member"
        );
    }
    let (status, still) = send(
        &p.lan,
        Method::GET,
        &format!("/api/v1/rides/{id}"),
        Some(&liz),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(still["state"]["state"], "awaiting_confirmation");
}

#[tokio::test]
async fn a_declined_fare_cannot_then_be_confirmed() {
    let p = pond().await;
    let phone = member_with_phone(&p, "liz3").await;
    let (_, quoted) = send(
        &p.lan,
        Method::POST,
        "/api/v1/rides/quote",
        Some(&phone),
        Some(trip()),
    )
    .await;
    let id = quoted["id"].as_str().unwrap();
    let (status, _) = send(
        &p.lan,
        Method::POST,
        &format!("/api/v1/rides/{id}/decline"),
        Some(&phone),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(
        &p.lan,
        Method::POST,
        &format!("/api/v1/rides/{id}/confirm"),
        Some(&phone),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn only_a_phone_paired_to_a_member_can_book_and_only_on_the_map() {
    let p = pond().await;
    // No device on the request: the pond's own desktop is not a member's phone.
    let (status, _) = send(
        &p.loopback,
        Method::POST,
        "/api/v1/rides/quote",
        None,
        Some(trip()),
    )
    .await;
    assert!(
        matches!(status, StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED),
        "{status}"
    );

    let phone = member_with_phone(&p, "liz4").await;
    let (status, _) = send(
        &p.lan,
        Method::POST,
        "/api/v1/rides/quote",
        Some(&phone),
        Some(json!({"pickup": {"latitude": 0.0, "longitude": 0.0}, "dropoff": {"latitude": 91.0, "longitude": 0.0}})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_drop_off_far_from_the_pickup_is_never_quoted() {
    let p = pond().await;
    let phone = member_with_phone(&p, "liz5").await;
    let (status, refused) = send(
        &p.lan,
        Method::POST,
        "/api/v1/rides/quote",
        Some(&phone),
        Some(json!({
            "pickup": {"latitude": -1.2676, "longitude": 36.8108},
            "dropoff": {"name": "Westlands", "latitude": 18.03, "longitude": -76.79},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert!(
        refused["error"].as_str().unwrap_or("").contains("too far"),
        "{refused}"
    );
}

#[tokio::test]
async fn while_travel_is_switched_off_no_fare_is_quoted_or_confirmed() {
    let p = pond().await;
    let phone = member_with_phone(&p, "liz6").await;
    let (_, quoted) = send(
        &p.lan,
        Method::POST,
        "/api/v1/rides/quote",
        Some(&phone),
        Some(trip()),
    )
    .await;
    let id = quoted["id"].as_str().unwrap();

    p.settings
        .set_key("ext_travel_enabled", "false".to_string())
        .await
        .unwrap();
    let (status, refused) = send(
        &p.lan,
        Method::POST,
        "/api/v1/rides/quote",
        Some(&phone),
        Some(trip()),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap_or("")
            .contains("switched off"),
        "{refused}"
    );
    let (status, _) = send(
        &p.lan,
        Method::POST,
        &format!("/api/v1/rides/{id}/confirm"),
        Some(&phone),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    // What was already quoted stays the member's to read and decline.
    let (status, still) = send(
        &p.lan,
        Method::GET,
        &format!("/api/v1/rides/{id}"),
        Some(&phone),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(still["state"]["state"], "awaiting_confirmation");
    let (status, _) = send(
        &p.lan,
        Method::POST,
        &format!("/api/v1/rides/{id}/decline"),
        Some(&phone),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// The member is whoever the phone's pairing names; a pairing that names nobody books nothing.
#[tokio::test]
async fn a_phone_paired_to_nobody_cannot_quote_or_confirm() {
    let p = pond().await;
    let liz = member_with_phone(&p, "liz7").await;
    let (_, quoted) = send(
        &p.lan,
        Method::POST,
        "/api/v1/rides/quote",
        Some(&liz),
        Some(trip()),
    )
    .await;
    let id = quoted["id"].as_str().unwrap();
    let stranger = phone_of_nobody(&p, "visitor").await;

    let (status, refused) = send(
        &p.lan,
        Method::POST,
        "/api/v1/rides/quote",
        Some(&stranger),
        Some(trip()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap_or("")
            .contains("not linked to a household member"),
        "{refused}"
    );
    let (status, _) = send(
        &p.lan,
        Method::POST,
        &format!("/api/v1/rides/{id}/confirm"),
        Some(&stranger),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (_, still) = send(
        &p.lan,
        Method::GET,
        &format!("/api/v1/rides/{id}"),
        Some(&liz),
        None,
    )
    .await;
    assert_eq!(still["state"]["state"], "awaiting_confirmation");
}

#[tokio::test]
async fn two_confirms_at_once_make_one_request() {
    let p = pond().await;
    let (liz, phone) = member_and_phone(&p, "liz8").await;
    let (_, quoted) = send(
        &p.lan,
        Method::POST,
        "/api/v1/rides/quote",
        Some(&phone),
        Some(trip()),
    )
    .await;
    let confirm = format!("/api/v1/rides/{}/confirm", quoted["id"].as_str().unwrap());

    let ((first, _), (second, _)) = tokio::join!(
        send(&p.lan, Method::POST, &confirm, Some(&phone), None),
        send(&p.lan, Method::POST, &confirm, Some(&phone), None),
    );
    let mut statuses = [first, second];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CONFLICT]);
    assert_eq!(PROVIDER.requests_for(&liz), 1);
}
