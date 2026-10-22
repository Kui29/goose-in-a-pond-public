//! Household roles: a phone acts with its member's role, the dashboard's host credential acts as
//! an owner, and every refusal is one `403 role_required` that says who may.
#[path = "support/connections_pond.rs"]
mod connections_pond;

use axum::http::{Method, StatusCode};
use connections_pond::*;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::domain::profile::{CreateProfileRequest, HouseholdRole};
use serde_json::{json, Value};

struct Phones {
    owner: String,
    member: String,
    guest: String,
    member_id: String,
    owner_id: String,
}

async fn person(pond: &Pond, name: &str, role: HouseholdRole) -> String {
    let profile = pond
        .state
        .profile_repo
        .create(CreateProfileRequest {
            display_name: name.into(),
            avatar_emoji: "X".into(),
        })
        .await
        .unwrap();
    pond.state
        .profile_repo
        .set_role(&profile.id, role)
        .await
        .unwrap();
    profile.id
}

/// An onboarded pond where Brian is the owner, Grace a member and Achieng a guest, each with a phone.
async fn phones(pond: &Pond) -> Phones {
    paired(pond, "install-claimant", None).await;
    pond.state
        .onboarding_repo
        .save_step(OnboardingStep::Completed)
        .await
        .unwrap();
    let owner_id = person(pond, "Brian", HouseholdRole::Owner).await;
    let member_id = person(pond, "Grace", HouseholdRole::Member).await;
    let guest_id = person(pond, "Achieng", HouseholdRole::Guest).await;
    Phones {
        owner: paired(pond, "install-brian", Some(&owner_id)).await,
        member: paired(pond, "install-grace", Some(&member_id)).await,
        guest: paired(pond, "install-achieng", Some(&guest_id)).await,
        member_id,
        owner_id,
    }
}

async fn call(
    pond: &Pond,
    method: Method,
    path: &str,
    body: Value,
    token: &str,
) -> (StatusCode, Value) {
    send(
        &pond.lan,
        method,
        path,
        Some(&body.to_string()),
        Some(token),
        None,
    )
    .await
}

async fn device_id(pond: &Pond, token: &str) -> String {
    let (status, body) = get(&pond.lan, "/api/v1/devices/self", token).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["device_id"].as_str().unwrap().to_string()
}

fn assert_refused(status: StatusCode, body: &Value, role: &str, allowed: Value) {
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "role_required");
    assert_eq!(body["role"], role);
    assert_eq!(body["allowed"], allowed);
    assert!(
        body["reason"].as_str().unwrap().starts_with("Only "),
        "{body}"
    );
}

#[tokio::test]
async fn every_profile_carries_its_role() {
    let pond = pond().await;
    let phones = phones(&pond).await;

    let (_, body) = get(&pond.lan, "/api/v1/profiles", &phones.member).await;
    let roles: Vec<(&str, &str)> = body["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["display_name"].as_str().unwrap(),
                p["role"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        roles,
        [
            ("Brian", "owner"),
            ("Grace", "member"),
            ("Achieng", "guest")
        ]
    );
}

#[tokio::test]
async fn only_an_owner_changes_settings() {
    let pond = pond().await;
    let phones = phones(&pond).await;
    let patch = json!({"assistant_name": "Bata"});

    let (status, body) = call(
        &pond,
        Method::PUT,
        "/api/v1/settings",
        patch.clone(),
        &phones.member,
    )
    .await;
    assert_refused(status, &body, "member", json!(["owner"]));

    let (status, body) = call(&pond, Method::PUT, "/api/v1/settings", patch, &phones.owner).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn the_dashboards_host_credential_acts_as_an_owner_whoever_its_token_belongs_to() {
    let pond = pond().await;
    let phones = phones(&pond).await;
    let (status, body) = send(
        &pond.host,
        Method::PUT,
        "/api/v1/settings",
        Some(r#"{"assistant_name": "Bata"}"#),
        Some(&phones.guest),
        Some(&pond.credential),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn members_run_schedules_and_guests_do_not() {
    let pond = pond().await;
    let phones = phones(&pond).await;
    let path = "/api/v1/schedules/no-such-schedule/pause";

    let (status, body) = call(&pond, Method::POST, path, json!({}), &phones.guest).await;
    assert_refused(
        status,
        &body,
        "guest",
        json!(["owner", "member", "developer"]),
    );

    // Past the gate, an unknown schedule is the scheduler's answer, not a refusal.
    let (status, body) = call(&pond, Method::POST, path, json!({}), &phones.member).await;
    assert_ne!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn switching_the_model_takes_an_owner_or_a_developer() {
    let pond = pond().await;
    let phones = phones(&pond).await;
    let path = "/api/v1/models/local/no-such-model/activate";

    let (status, body) = call(
        &pond,
        Method::POST,
        path,
        json!({"role": "chat"}),
        &phones.member,
    )
    .await;
    assert_refused(status, &body, "member", json!(["owner", "developer"]));

    pond.state
        .profile_repo
        .set_role(&phones.member_id, HouseholdRole::Developer)
        .await
        .unwrap();
    let (status, body) = call(
        &pond,
        Method::POST,
        path,
        json!({"role": "chat"}),
        &phones.member,
    )
    .await;
    assert_ne!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn anyone_may_remove_their_own_phone_but_only_an_owner_someone_elses() {
    let pond = pond().await;
    let phones = phones(&pond).await;
    let owners_phone = device_id(&pond, &phones.owner).await;
    let guests_phone = device_id(&pond, &phones.guest).await;

    let path = format!("/api/v1/devices/{owners_phone}");
    let (status, body) = call(&pond, Method::DELETE, &path, json!({}), &phones.member).await;
    assert_refused(status, &body, "member", json!(["owner"]));

    let path = format!("/api/v1/devices/{guests_phone}");
    let (status, body) = call(&pond, Method::DELETE, &path, json!({}), &phones.guest).await;
    assert_ne!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn an_owner_sets_roles_and_the_household_always_keeps_one_owner() {
    let pond = pond().await;
    let phones = phones(&pond).await;
    let role = |id: &str| format!("/api/v1/profiles/{id}/role");

    let (status, body) = call(
        &pond,
        Method::PATCH,
        &role(&phones.owner_id),
        json!({"role": "member"}),
        &phones.member,
    )
    .await;
    assert_refused(status, &body, "member", json!(["owner"]));

    let (status, body) = call(
        &pond,
        Method::PATCH,
        &role(&phones.owner_id),
        json!({"role": "member"}),
        &phones.owner,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "last_owner");

    let (status, body) = call(
        &pond,
        Method::PATCH,
        &role(&phones.member_id),
        json!({"role": "owner"}),
        &phones.owner,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["role"], "owner");

    // With a second owner, the first may step down.
    let (status, body) = call(
        &pond,
        Method::PATCH,
        &role(&phones.owner_id),
        json!({"role": "member"}),
        &phones.owner,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = call(
        &pond,
        Method::PATCH,
        &role(&phones.member_id),
        json!({"role": "king"}),
        &phones.member,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "unknown_role");
}

#[tokio::test]
async fn the_last_owner_cannot_be_deleted_while_others_remain() {
    let pond = pond().await;
    let phones = phones(&pond).await;
    let path = format!("/api/v1/profiles/{}", phones.owner_id);
    let (status, body) = call(&pond, Method::DELETE, &path, json!({}), &phones.owner).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "last_owner");
}

#[tokio::test]
async fn a_member_edits_their_own_preferences_but_not_anothers() {
    let pond = pond().await;
    let phones = phones(&pond).await;
    let prefs = json!({"preferences": {"language": "sw"}});

    let own = format!("/api/v1/profiles/{}", phones.member_id);
    let (status, body) = call(&pond, Method::PATCH, &own, prefs.clone(), &phones.member).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let others = format!("/api/v1/profiles/{}", phones.owner_id);
    let (status, body) = call(&pond, Method::PATCH, &others, prefs, &phones.member).await;
    assert_refused(status, &body, "member", json!(["owner"]));
}
