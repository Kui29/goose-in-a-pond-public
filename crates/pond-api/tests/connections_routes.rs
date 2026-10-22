//! `docs/architecture/pond-connections.md`: the connections routes answer any paired token in
//! every state and never a caller without one; only the host, the phone that claimed the pond or
//! an owner's, member's or developer's device may change the network; each station refusal keeps
//! its code.
#[path = "support/connections_pond.rs"]
mod connections_pond;

use std::sync::Arc;

use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, StatusCode};
use connections_pond::*;
use pond_api::test_support::{pair_phone, TEST_SPKI_PIN};
use pond_core::network::domain::wifi::{
    WifiCapabilities, WifiForgetError, WifiJoinError, WifiScanError,
};
use pond_core::network::ports::wifi_station::WifiStation;
use pond_core::security::ports::handshake::Handshake;
use serde_json::json;

const NOT_A_MEMBER: &str =
    "This device belongs to no one in the household, so it cannot change the pond's network";

const ROUTES: [(&str, &str); 5] = [
    ("GET", "/api/v1/connections"),
    ("GET", "/api/v1/connections/wifi/networks"),
    ("POST", "/api/v1/connections/wifi/join"),
    ("DELETE", "/api/v1/connections/wifi/networks/Attic"),
    ("GET", "/api/v1/setup/status"),
];

#[tokio::test]
async fn no_route_answers_a_caller_without_a_token_not_even_the_host() {
    let pond = pond().await;
    for (method, path) in ROUTES {
        let method: Method = method.parse().unwrap();
        for (router, credential) in [
            (&pond.lan, None),
            (&pond.host, None),
            (&pond.host, Some(&pond.credential)),
        ] {
            let (status, body) = send(
                router,
                method.clone(),
                path,
                Some(r#"{"ssid": "Cafe"}"#),
                None,
                credential,
            )
            .await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{method} {path}, host credential {}: {body}",
                credential.is_some()
            );
        }
    }
    assert!(pond.station.joins().is_empty());
    assert!(pond.station.forgotten().is_empty());
}

#[tokio::test]
async fn one_read_shows_every_link_and_whether_this_caller_may_change_them() {
    let pond = pond().await;
    let phones = household(&pond).await;

    let (status, body) = get(&pond.lan, "/api/v1/connections", &phones.claimant).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({
            "hostname": "nano",
            "https_port": 4443,
            "wifi": {
                "state": "connected",
                "reason": null,
                "ssid": "Home",
                "address": "192.168.1.42",
                "signal": 70,
                "capabilities": {
                    "scan": {"available": true, "reason": null},
                    "join": {"available": true, "reason": null},
                    "forget": {"available": true, "reason": null},
                },
            },
            "ethernet": [
                {"interface": "eth0", "link": "up", "address": "192.168.1.20", "speed_mbps": 1000},
                {"interface": "eth1", "link": "down", "address": null, "speed_mbps": null},
            ],
            "bluetooth": {
                "enabled": true,
                "state": "advertising",
                "reason": null,
                "advertising_as": "Pond 7f3a",
                "session_open": false,
            },
            "can_change": {"allowed": true, "reason": null},
        })
    );

    let (status, body) = get(&pond.lan, "/api/v1/connections", &phones.guest).await;
    assert_eq!(status, StatusCode::OK, "a guest may look: {body}");
    assert_eq!(
        body["can_change"],
        json!({"allowed": false, "reason": NOT_A_MEMBER})
    );
    let (_, body) = get(&pond.lan, "/api/v1/connections", &phones.member).await;
    assert_eq!(body["can_change"], json!({"allowed": true, "reason": null}));

    pond.station.forget("Home").await.unwrap();
    pond.station.set_capabilities(WifiCapabilities::unsupported(
        "this host has no Wi-Fi adapter",
    ));
    let (_, body) = get(&pond.lan, "/api/v1/connections", &phones.guest).await;
    let none = json!({"available": false, "reason": "this host has no Wi-Fi adapter"});
    assert_eq!(
        body["wifi"],
        json!({
            "state": "unavailable",
            "reason": "this host has no Wi-Fi adapter",
            "ssid": null, "address": null, "signal": null,
            "capabilities": {"scan": none, "join": none, "forget": none},
        })
    );

    let (status, body) = get(&pond.bare, "/api/v1/connections", &phones.guest).await;
    assert_eq!(
        (status, body),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "unsupported"})
        ),
        "a server wired without a station says so"
    );
}

#[tokio::test]
async fn an_unattributed_phone_that_did_not_claim_the_pond_may_not_change_it() {
    let pond = pond().await;
    let phones = household(&pond).await;
    let body = json!({"ssid": "Home", "passphrase": PASSPHRASE}).to_string();
    assert_eq!(
        join(&pond.lan, &phones.guest, &body).await,
        (StatusCode::FORBIDDEN, json!({"error": "not_allowed"}))
    );
    assert_eq!(
        join(&pond.lan, &phones.guest, "not even json").await,
        (StatusCode::FORBIDDEN, json!({"error": "not_allowed"})),
        "the gate comes before the body"
    );
    for path in [
        "/api/v1/connections/wifi/networks/Attic",
        "/api/v1/connections/wifi/networks/Home?confirm=true",
    ] {
        assert_eq!(
            forget(&pond.lan, &phones.guest, path).await,
            (StatusCode::FORBIDDEN, json!({"error": "not_allowed"})),
            "{path}"
        );
    }
    assert!(pond.station.joins().is_empty());
    assert!(pond.station.forgotten().is_empty());
}

#[tokio::test]
async fn the_claimant_a_member_and_the_host_may_change_it() {
    let pond = pond().await;
    let phones = household(&pond).await;

    let (status, body) = join(&pond.lan, &phones.claimant, r#"{"ssid": "Cafe"}"#).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({"ssid": "Cafe", "address": "192.168.1.42", "https_port": 4443,
               "hostname": "nano"})
    );

    let body = json!({"ssid": "Home", "passphrase": PASSPHRASE, "hidden": false}).to_string();
    let (status, _) = join(&pond.lan, &phones.member, &body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        pond.station.joins().last().unwrap().passphrase.as_deref(),
        Some(PASSPHRASE),
        "the passphrase reached the station intact"
    );

    let (status, body) = send(
        &pond.host,
        Method::DELETE,
        "/api/v1/connections/wifi/networks/Attic",
        None,
        Some(&phones.guest),
        Some(&pond.credential),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the host credential makes any token a changer: {body}"
    );
    let (_, body) = send(
        &pond.host,
        Method::GET,
        "/api/v1/connections",
        None,
        Some(&phones.guest),
        Some(&pond.credential),
    )
    .await;
    assert_eq!(body["can_change"], json!({"allowed": true, "reason": null}));
    assert_eq!(pond.station.forgotten(), ["Attic"]);
}

#[tokio::test]
async fn every_join_refusal_keeps_its_code_and_status() {
    let pond = pond().await;
    let phones = household(&pond).await;
    let token = &phones.claimant;
    let home = json!({"ssid": "Home", "passphrase": PASSPHRASE}).to_string();

    for (error, status) in [
        (WifiJoinError::WrongPassword, StatusCode::UNAUTHORIZED),
        (WifiJoinError::NotFound, StatusCode::NOT_FOUND),
        (WifiJoinError::Timeout, StatusCode::GATEWAY_TIMEOUT),
        (WifiJoinError::NoAddress, StatusCode::BAD_GATEWAY),
        (WifiJoinError::NotPermitted, StatusCode::FORBIDDEN),
        (WifiJoinError::Unsupported, StatusCode::SERVICE_UNAVAILABLE),
        (WifiJoinError::Busy, StatusCode::CONFLICT),
        (WifiJoinError::UnsupportedSecurity, StatusCode::BAD_REQUEST),
    ] {
        pond.station.fail_joins(Some(error));
        assert_eq!(
            join(&pond.lan, token, &home).await,
            (status, json!({"error": error.code()})),
            "{error:?}"
        );
    }
    pond.station.fail_joins(None);

    let office = json!({"ssid": "Office", "passphrase": PASSPHRASE}).to_string();
    assert_eq!(
        join(&pond.lan, token, &office).await,
        (
            StatusCode::BAD_REQUEST,
            json!({"error": "unsupported_security"})
        ),
        "an enterprise network is refused by its security, before any attempt"
    );
    let wrong = json!({"ssid": "Home", "passphrase": "not-the-passphrase"}).to_string();
    assert_eq!(
        join(&pond.lan, token, &wrong).await,
        (StatusCode::UNAUTHORIZED, json!({"error": "wrong_password"}))
    );
    assert_eq!(
        join(&pond.lan, token, r#"{"ssid": "Nowhere"}"#).await,
        (StatusCode::NOT_FOUND, json!({"error": "not_found"}))
    );

    let long_ssid = "x".repeat(33);
    let long_passphrase = "y".repeat(65);
    for body in [
        r#"{"ssid": ""}"#.to_string(),
        json!({ "ssid": long_ssid }).to_string(),
        r#"{"ssid": "Home", "passphrase": ""}"#.to_string(),
        json!({"ssid": "Home", "passphrase": long_passphrase}).to_string(),
        r#"{"ssid": "Home", "passphrase": 12345678}"#.to_string(),
        r#"{"passphrase": "otter-lantern"}"#.to_string(),
        "{".to_string(),
        String::new(),
    ] {
        assert_eq!(
            join(&pond.lan, token, &body).await,
            (StatusCode::BAD_REQUEST, json!({"error": "invalid_request"})),
            "{body}"
        );
    }
}

#[tokio::test]
async fn forgetting_the_network_the_pond_is_on_needs_confirm() {
    let pond = pond().await;
    let phones = household(&pond).await;
    let token = &phones.claimant;

    assert_eq!(
        forget(&pond.lan, token, "/api/v1/connections/wifi/networks/Home").await,
        (StatusCode::CONFLICT, json!({"error": "in_use"}))
    );
    assert_eq!(
        forget(
            &pond.lan,
            token,
            "/api/v1/connections/wifi/networks/Home?confirm=false"
        )
        .await,
        (StatusCode::CONFLICT, json!({"error": "in_use"}))
    );
    assert!(pond.station.forgotten().is_empty());
    assert_eq!(
        forget(
            &pond.lan,
            token,
            "/api/v1/connections/wifi/networks/Home?confirm=true"
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        forget(
            &pond.lan,
            token,
            "/api/v1/connections/wifi/networks/Flat%202%2FB"
        )
        .await
        .0,
        StatusCode::NO_CONTENT,
        "the SSID is percent-decoded, a slash included"
    );
    assert_eq!(pond.station.forgotten(), ["Home", "Flat 2/B"]);
    assert_eq!(
        forget(
            &pond.lan,
            token,
            "/api/v1/connections/wifi/networks/Nowhere"
        )
        .await,
        (StatusCode::NOT_FOUND, json!({"error": "not_found"}))
    );
    let long = format!("/api/v1/connections/wifi/networks/{}", "x".repeat(33));
    for path in [
        long.as_str(),
        "/api/v1/connections/wifi/networks/Attic?confirm=maybe",
    ] {
        assert_eq!(
            forget(&pond.lan, token, path).await,
            (StatusCode::BAD_REQUEST, json!({"error": "invalid_request"})),
            "{path}"
        );
    }
    for (error, status) in [
        (WifiForgetError::NotPermitted, StatusCode::FORBIDDEN),
        (
            WifiForgetError::Unsupported,
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (WifiForgetError::Busy, StatusCode::CONFLICT),
    ] {
        pond.station.fail_forgets(Some(error));
        assert_eq!(
            forget(&pond.lan, token, "/api/v1/connections/wifi/networks/Attic").await,
            (status, json!({"error": error.code()})),
            "{error:?}"
        );
    }
}

#[tokio::test]
async fn a_scan_is_ranked_marks_known_and_current_and_keeps_its_codes() {
    let pond = pond().await;
    let phones = household(&pond).await;

    let (status, body) = get(
        &pond.lan,
        "/api/v1/connections/wifi/networks",
        &phones.guest,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "any paired token may scan");
    assert_eq!(
        body,
        json!({"networks": [
            {"ssid": "Home", "security": "wpa2", "signal": 70, "known": true, "current": true},
            {"ssid": "Office", "security": "enterprise", "signal": 55, "known": false,
             "current": false},
            {"ssid": "Cafe", "security": "open", "signal": 40, "known": false, "current": false},
        ]})
    );

    for (error, status) in [
        (WifiScanError::Unsupported, StatusCode::SERVICE_UNAVAILABLE),
        (WifiScanError::NotPermitted, StatusCode::FORBIDDEN),
        (WifiScanError::Busy, StatusCode::CONFLICT),
    ] {
        pond.station.fail_scans(Some(error));
        assert_eq!(
            get(
                &pond.lan,
                "/api/v1/connections/wifi/networks",
                &phones.guest
            )
            .await,
            (status, json!({"error": error.code()})),
            "{error:?}"
        );
    }
}

#[tokio::test]
async fn the_setup_status_says_who_claimed_the_pond_and_never_its_code() {
    let pond = pond().await;
    let phones = household(&pond).await;

    let (status, body) = get(&pond.lan, "/api/v1/setup/status", &phones.claimant).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], "7f3a");
    assert_eq!(body["claimed"], true);
    assert_eq!(body["claimed_by_this_device"], true);
    let claimed_at = body["claimed_at"].as_str().unwrap();
    assert!(
        claimed_at.ends_with('Z') && claimed_at.len() == "2026-10-18T18:06:12Z".len(),
        "{claimed_at}"
    );
    assert_eq!(
        body.as_object().unwrap().len(),
        4,
        "nothing else, the code least of all: {body}"
    );

    let (_, body) = get(&pond.lan, "/api/v1/setup/status", &phones.member).await;
    assert_eq!(body["claimed_by_this_device"], false);

    let (_, body) = get(&pond.bare, "/api/v1/setup/status", &phones.guest).await;
    assert_eq!(body["claimed_by_this_device"], false);
}

#[tokio::test]
async fn a_pond_with_no_setup_record_has_no_card_to_show() {
    let pond_api::test_support::TestState {
        state,
        handshake,
        dir: _dir,
    } = pond_api::test_support::app_state().await;
    let router = pond_api::build_router(Arc::new(state), "pond-desktop/dist".into())
        .layer(MockConnectInfo(host_peer()));
    let code = handshake.issue_pairing_code().await.unwrap().code;
    let (_, paired) = pair_phone(&router, &code, "install-solo", TEST_SPKI_PIN).await;
    let token = paired["session_token"].as_str().unwrap();
    assert_eq!(
        get(&router, "/api/v1/setup/status", token).await,
        (
            StatusCode::OK,
            json!({"id": null, "claimed": false, "claimed_at": null,
                   "claimed_by_this_device": false})
        )
    );
}
