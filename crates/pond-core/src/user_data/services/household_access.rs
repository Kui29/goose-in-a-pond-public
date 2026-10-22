//! Who in the household may do what. A phone acts with the role of the member it belongs to;
//! the pond's own dashboard, and the device that set the pond up, act as an owner.

use crate::user_data::domain::profile::HouseholdRole;
use serde::Serialize;

/// The wire code every refusal answers with.
pub const ROLE_REQUIRED: &str = "role_required";

/// Who is asking, as far as permissions go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// The host credential, or a tokenless caller while the pond is not yet onboarded.
    Host,
    /// The device that claimed the pond, while it belongs to no member yet.
    Claimant,
    Member {
        profile_id: String,
        role: HouseholdRole,
    },
    /// A paired device that belongs to no member while the pond is not yet onboarded: no roles
    /// exist yet, so it runs setup as before, but it still may not change the network.
    Setup,
    /// A paired device that belongs to no member: it may look but not change.
    Unattributed,
    NoDevice,
    /// The device's member could not be read. Access narrows; it never assumes a member.
    Unreadable,
}

impl Caller {
    pub fn role(&self) -> Option<HouseholdRole> {
        match self {
            Self::Member { role, .. } => Some(*role),
            _ => None,
        }
    }

    pub fn profile_id(&self) -> Option<&str> {
        match self {
            Self::Member { profile_id, .. } => Some(profile_id),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Add, edit or remove people and their roles.
    ManageHousehold,
    /// Remove a phone that is not the caller's own.
    RemoveOtherDevice,
    ChangeSettings,
    ChangeModel,
    ManageSchedules,
    EnrollRemoteAccess,
    ChangeConnections,
}

impl Action {
    /// What the action does, completing "can …" in a refusal's sentence.
    fn phrase(self) -> &'static str {
        match self {
            Self::ManageHousehold => "add or change people in the household",
            Self::RemoveOtherDevice => "remove someone else's device",
            Self::ChangeSettings => "change the pond's settings",
            Self::ChangeModel => "switch the AI model",
            Self::ManageSchedules => "change schedules",
            Self::EnrollRemoteAccess => "use the pond from outside the home",
            Self::ChangeConnections => "change the pond's network",
        }
    }

    pub fn allowed_roles(self) -> &'static [HouseholdRole] {
        use HouseholdRole::*;
        match self {
            Self::ManageHousehold | Self::RemoveOtherDevice | Self::ChangeSettings => &[Owner],
            Self::ChangeModel => &[Owner, Developer],
            Self::ManageSchedules | Self::EnrollRemoteAccess | Self::ChangeConnections => {
                &[Owner, Member, Developer]
            }
        }
    }
}

/// Why the caller may not; serialised as the 403 body beside `"error": "role_required"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Refusal {
    pub action: Action,
    /// The caller's role; `None` for a caller that is no member.
    pub role: Option<HouseholdRole>,
    pub allowed: &'static [HouseholdRole],
    /// The device's member could not be read, so trying again may succeed.
    #[serde(skip)]
    pub unreadable: bool,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let action = self.action.phrase();
        if self.unreadable {
            return write!(
                f,
                "The pond could not read whose device this is, so it cannot {action}; try again"
            );
        }
        let allowed: Vec<&str> = self.allowed.iter().map(|role| role.as_str()).collect();
        let who = match allowed.as_slice() {
            [only] => format!("an {only}"),
            [rest @ .., last] => format!("an {} or {last}", rest.join(", ")),
            [] => "nobody".to_string(),
        };
        match self.role {
            Some(role) => write!(
                f,
                "Only {who} can {action}; this device belongs to a {}",
                role.as_str()
            ),
            None => write!(
                f,
                "This device belongs to no one in the household, so it cannot {action}"
            ),
        }
    }
}

pub fn may(caller: &Caller, action: Action) -> Result<(), Refusal> {
    let allowed = action.allowed_roles();
    let permitted = match caller {
        Caller::Host | Caller::Claimant => true,
        Caller::Member { role, .. } => allowed.contains(role),
        Caller::Setup => action != Action::ChangeConnections,
        Caller::Unattributed | Caller::NoDevice | Caller::Unreadable => false,
    };
    if permitted {
        return Ok(());
    }
    Err(Refusal {
        action,
        role: caller.role(),
        allowed,
        unreadable: *caller == Caller::Unreadable,
    })
}

/// Whether `caller` may change `target_profile_id`'s own details: themselves, or a manager.
pub fn may_edit_profile(caller: &Caller, target_profile_id: &str) -> Result<(), Refusal> {
    if caller.profile_id() == Some(target_profile_id) {
        return Ok(());
    }
    may(caller, Action::ManageHousehold)
}

#[cfg(test)]
mod tests {
    use super::*;
    use HouseholdRole::*;

    fn member(role: HouseholdRole) -> Caller {
        Caller::Member {
            profile_id: "grace".into(),
            role,
        }
    }

    const EVERY_ACTION: [Action; 7] = [
        Action::ManageHousehold,
        Action::RemoveOtherDevice,
        Action::ChangeSettings,
        Action::ChangeModel,
        Action::ManageSchedules,
        Action::EnrollRemoteAccess,
        Action::ChangeConnections,
    ];

    #[test]
    fn the_host_and_an_unattributed_claimant_may_do_everything() {
        for action in EVERY_ACTION {
            assert_eq!(may(&Caller::Host, action), Ok(()));
            assert_eq!(may(&Caller::Claimant, action), Ok(()));
        }
    }

    #[test]
    fn during_setup_an_unattributed_phone_may_do_all_but_change_the_network() {
        for action in EVERY_ACTION {
            assert_eq!(
                may(&Caller::Setup, action).is_ok(),
                action != Action::ChangeConnections,
                "{action:?}"
            );
        }
    }

    #[test]
    fn an_owner_may_do_everything() {
        for action in EVERY_ACTION {
            assert_eq!(may(&member(Owner), action), Ok(()));
        }
    }

    #[test]
    fn a_member_runs_the_house_but_not_the_household() {
        let grace = member(Member);
        for action in [
            Action::ManageSchedules,
            Action::EnrollRemoteAccess,
            Action::ChangeConnections,
        ] {
            assert_eq!(may(&grace, action), Ok(()));
        }
        for action in [
            Action::ManageHousehold,
            Action::RemoveOtherDevice,
            Action::ChangeSettings,
            Action::ChangeModel,
        ] {
            assert!(may(&grace, action).is_err(), "{action:?}");
        }
    }

    #[test]
    fn a_developer_may_also_switch_the_model() {
        assert_eq!(may(&member(Developer), Action::ChangeModel), Ok(()));
        assert!(may(&member(Developer), Action::ChangeSettings).is_err());
    }

    #[test]
    fn a_guest_and_every_non_member_may_change_nothing() {
        for caller in [
            member(Guest),
            Caller::Unattributed,
            Caller::NoDevice,
            Caller::Unreadable,
        ] {
            for action in EVERY_ACTION {
                assert!(may(&caller, action).is_err(), "{caller:?} {action:?}");
            }
        }
    }

    #[test]
    fn a_refusal_names_the_callers_role_and_who_may() {
        assert_eq!(
            may(&member(Guest), Action::ManageSchedules),
            Err(Refusal {
                action: Action::ManageSchedules,
                role: Some(Guest),
                allowed: &[Owner, Member, Developer],
                unreadable: false,
            })
        );
        assert_eq!(
            may(&Caller::Unattributed, Action::ChangeSettings)
                .unwrap_err()
                .role,
            None
        );
    }

    #[test]
    fn a_refusal_reads_as_a_sentence_about_who_may() {
        assert_eq!(
            may(&member(Guest), Action::ManageSchedules).unwrap_err().to_string(),
            "Only an owner, member or developer can change schedules; this device belongs to a guest"
        );
        assert_eq!(
            may(&member(Member), Action::ChangeSettings)
                .unwrap_err()
                .to_string(),
            "Only an owner can change the pond's settings; this device belongs to a member"
        );
        assert_eq!(
            may(&Caller::Unattributed, Action::ChangeConnections).unwrap_err().to_string(),
            "This device belongs to no one in the household, so it cannot change the pond's network"
        );
        assert!(may(&Caller::Unreadable, Action::ChangeConnections)
            .unwrap_err()
            .to_string()
            .ends_with("try again"));
    }

    #[test]
    fn anyone_may_edit_their_own_profile_but_only_an_owner_anothers() {
        assert_eq!(may_edit_profile(&member(Guest), "grace"), Ok(()));
        assert!(may_edit_profile(&member(Member), "brian").is_err());
        assert_eq!(may_edit_profile(&member(Owner), "brian"), Ok(()));
        assert!(may_edit_profile(&Caller::Unattributed, "grace").is_err());
    }
}
