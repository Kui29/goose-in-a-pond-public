//! Durable remote revocations, and the record of which phones were ever enrolled.
//!
//! A revocation is written before anything is sent, so it survives an offline coordinator and
//! a restart. Each entry backs off on its own: one device the coordinator keeps refusing must
//! not hold up the others behind it. A revocation for a device this Pond never enrolled would
//! leave a tombstone on the coordinator, so only recorded devices are revoked outright; on a
//! Pond enrolled before the record existed, an unrecorded device is checked first.

use anyhow::{ensure, Result};
use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

pub(super) const QUEUE_FILE: &str = "revocations.json";
pub(super) const ENROLLED_FILE: &str = "enrolled.json";
/// The machine key each enrolled phone uses, by network-hashed id; a cache of the coordinator's.
pub(super) const MACHINES_FILE: &str = "machines.json";
/// Queue and record both hold at most this many devices.
pub(super) const MAX_DEVICES: usize = 256;
/// Revocations attempted per 30-second tick.
pub(super) const PER_TICK: usize = 4;
const FIRST_RETRY_SECS: i64 = 30;
const LONGEST_RETRY_SECS: i64 = 6 * 60 * 60;

/// A queued revocation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Pending {
    pub queued_at: DateTime<Utc>,
    pub attempts: u32,
    pub next_attempt: DateTime<Utc>,
    /// Ask the coordinator whether the device exists before revoking it.
    pub verify: bool,
}

impl Pending {
    pub fn new(verify: bool, now: DateTime<Utc>) -> Self {
        Self {
            queued_at: now,
            attempts: 0,
            next_attempt: now,
            verify,
        }
    }

    /// Double the wait after each failure, up to six hours.
    pub fn back_off(&mut self, now: DateTime<Utc>) {
        let wait = FIRST_RETRY_SECS
            .saturating_mul(1i64 << self.attempts.min(20))
            .min(LONGEST_RETRY_SECS);
        self.attempts = self.attempts.saturating_add(1);
        self.next_attempt = now + chrono::Duration::seconds(wait);
    }
}

/// Device (network-hashed) to its queued revocation.
pub(super) type Queue = BTreeMap<String, Pending>;

/// The queue before per-entry backoff was a plain set of devices.
#[derive(Deserialize)]
#[serde(untagged)]
enum OnDisk {
    Current(Queue),
    Legacy(BTreeSet<String>),
}

pub(super) fn load_queue(directory: &Path, now: DateTime<Utc>) -> Result<Queue> {
    let queue = match read_private_json::<OnDisk>(&directory.join(QUEUE_FILE), 65536)? {
        None => Queue::new(),
        Some(OnDisk::Current(queue)) => queue,
        // They were queued as enrolled devices, so they are revoked without a check.
        Some(OnDisk::Legacy(devices)) => devices
            .into_iter()
            .map(|device| (device, Pending::new(false, now)))
            .collect(),
    };
    ensure!(
        queue.len() <= MAX_DEVICES && queue.keys().all(|id| super::valid_device(id)),
        "invalid remote revocation entries"
    );
    Ok(queue)
}

/// Write the queue. An empty one is written as the empty set a binary from before per-entry
/// backoff reads, so a Pond whose queue has drained can be downgraded.
pub(super) fn save_queue(directory: &Path, queue: &Queue) -> Result<()> {
    if queue.is_empty() {
        return write_private_json(directory, QUEUE_FILE, &BTreeSet::<String>::new());
    }
    write_private_json(directory, QUEUE_FILE, queue)
}

/// Entries whose next attempt has come, soonest first, then oldest first.
pub(super) fn due(queue: &Queue, now: DateTime<Utc>) -> Vec<(String, Pending)> {
    let mut due: Vec<_> = queue
        .iter()
        .filter(|(_, entry)| entry.next_attempt <= now)
        .map(|(device, entry)| (device.clone(), entry.clone()))
        .collect();
    due.sort_by_key(|(_, entry)| (entry.next_attempt, entry.queued_at));
    due.truncate(PER_TICK);
    due
}

/// Phones this Pond enrolled, by network-hashed id.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Enrolled {
    /// True when the record has existed since before this household's first enrollment, so an
    /// absent device was never enrolled. False for a Pond that enrolled phones before it.
    pub complete: bool,
    pub devices: BTreeSet<String>,
}

/// The enrollment record. One that cannot be read or does not validate (corrupt, or left `0644`
/// by a restore) is moved aside as `enrolled.json.corrupt-<unix time>` and read as an incomplete
/// record: refusing it would answer every removal, logout and enrollment 503 until someone
/// repaired the file, and an incomplete record only makes removals check before they revoke.
pub(super) fn load_enrolled(directory: &Path) -> Enrolled {
    let path = directory.join(ENROLLED_FILE);
    let reason = match read_private_json::<Enrolled>(&path, 65536) {
        Ok(record) => {
            let record = record.unwrap_or_default();
            if record.devices.len() <= MAX_DEVICES
                && record.devices.iter().all(|id| super::valid_device(id))
            {
                return record;
            }
            "invalid enrollment record".to_owned()
        }
        Err(error) => format!("{error:#}"),
    };
    let aside = directory.join(format!(
        "{ENROLLED_FILE}.corrupt-{}",
        Utc::now().timestamp()
    ));
    match std::fs::rename(&path, &aside) {
        Ok(()) => tracing::warn!(
            target: "giap::trace",
            kind = "enrollment_record_replaced",
            %reason,
            aside = %aside.display(),
            "the enrollment record was unreadable; it was moved aside and removals now check \
             with the coordinator before revoking"
        ),
        Err(error) => tracing::warn!(
            target: "giap::trace",
            kind = "enrollment_record_replaced",
            %reason,
            rename_error = %error,
            "the enrollment record was unreadable and could not be moved aside; removals now \
             check with the coordinator before revoking"
        ),
    }
    Enrolled::default()
}

/// How a revocation should be sent, given what this Pond knows it enrolled.
#[derive(Debug, PartialEq)]
pub(super) enum Plan {
    Revoke,
    VerifyThenRevoke,
    NothingToRevoke,
}

/// The machine cache. An unreadable one is an error rather than empty, so a tailnet request is
/// refused, never let through on a guess.
pub(super) fn load_machines(directory: &Path) -> Result<BTreeMap<String, String>> {
    Ok(read_private_json(&directory.join(MACHINES_FILE), 65536)?.unwrap_or_default())
}

pub(super) fn plan(record: &Enrolled, device: &str) -> Plan {
    if record.devices.contains(device) {
        Plan::Revoke
    } else if record.complete {
        Plan::NothingToRevoke
    } else {
        Plan::VerifyThenRevoke
    }
}

/// Read a private JSON file: a regular `0600`-or-stricter file, not a symlink, within `max`.
/// `None` when it does not exist. Opened with `O_NOFOLLOW` and checked on the open handle, so
/// a symlink swapped in after a path check is never followed.
pub(super) fn read_private_json<T: DeserializeOwned>(path: &Path, max: u64) -> Result<Option<T>> {
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        other => other?,
    };
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o077 == 0 && metadata.len() <= max,
        "{} is not a private file of plausible size",
        path.display()
    );
    let mut bytes = Vec::new();
    Read::take(&mut file, max + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= max,
        "{} grew while being read",
        path.display()
    );
    Ok(Some(serde_json::from_slice(&bytes)?))
}

/// Atomically replace `directory/name` with `value`, mode `0600`, and sync the directory.
pub(super) fn write_private_json<T: Serialize>(
    directory: &Path,
    name: &str,
    value: &T,
) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.as_file().sync_all()?;
    file.persist(directory.join(name))?;
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + seconds, 0).unwrap()
    }

    const PHONE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn a_failing_entry_waits_longer_each_time_up_to_six_hours() {
        let mut entry = Pending::new(false, at(0));
        let mut waits = Vec::new();
        for _ in 0..12 {
            entry.back_off(at(0));
            waits.push((entry.next_attempt - at(0)).num_seconds());
        }
        assert_eq!(&waits[..4], &[30, 60, 120, 240]);
        assert_eq!(*waits.last().unwrap(), 6 * 60 * 60);
        assert_eq!(entry.attempts, 12);
    }

    #[test]
    fn a_backed_off_entry_does_not_hold_up_the_others() {
        let mut stuck = Pending::new(false, at(0));
        stuck.back_off(at(0));
        let queue = Queue::from([
            (PHONE.to_owned(), stuck),
            (OTHER.to_owned(), Pending::new(false, at(10))),
        ]);
        let due = due(&queue, at(20));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].0, OTHER);
        assert_eq!(super::due(&queue, at(40)).len(), 2);
    }

    #[test]
    fn only_recorded_devices_are_revoked_without_a_check() {
        let mut record = Enrolled::default();
        assert_eq!(plan(&record, PHONE), Plan::VerifyThenRevoke);
        record.devices.insert(PHONE.to_owned());
        assert_eq!(plan(&record, PHONE), Plan::Revoke);
        record.complete = true;
        assert_eq!(plan(&record, OTHER), Plan::NothingToRevoke);
        assert_eq!(plan(&record, PHONE), Plan::Revoke);
    }

    #[test]
    fn a_legacy_queue_is_read_as_enrolled_devices_due_now() {
        let dir = tempfile::tempdir().unwrap();
        write_private_json(dir.path(), QUEUE_FILE, &BTreeSet::from([PHONE])).unwrap();
        let queue = load_queue(dir.path(), at(5)).unwrap();
        assert_eq!(queue[PHONE], Pending::new(false, at(5)));
        write_private_json(dir.path(), QUEUE_FILE, &queue).unwrap();
        assert_eq!(load_queue(dir.path(), at(9)).unwrap(), queue);
    }

    #[test]
    fn an_empty_queue_is_written_as_an_older_binary_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        save_queue(dir.path(), &Queue::new()).unwrap();
        let written = std::fs::read(dir.path().join(QUEUE_FILE)).unwrap();
        assert_eq!(written, b"[]");
        assert!(serde_json::from_slice::<BTreeSet<String>>(&written).is_ok());
        assert!(load_queue(dir.path(), at(0)).unwrap().is_empty());

        let queue = Queue::from([(PHONE.to_owned(), Pending::new(true, at(0)))]);
        save_queue(dir.path(), &queue).unwrap();
        assert_eq!(load_queue(dir.path(), at(1)).unwrap(), queue);
    }

    #[test]
    fn private_files_refuse_symlinks_loose_modes_and_bulk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        assert!(read_private_json::<u8>(&path, 16).unwrap().is_none());
        write_private_json(dir.path(), "x.json", &7u8).unwrap();
        assert_eq!(read_private_json::<u8>(&path, 16).unwrap(), Some(7));
        assert!(read_private_json::<u8>(&path, 0).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_private_json::<u8>(&path, 16).is_err());
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_private_json::<u8>(&link, 16).is_err());
    }
}
