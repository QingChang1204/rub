use super::{
    RegistryData, RegistryEntry, liveness::registry_entry_snapshot_for_home, read_registry,
};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::Path;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryEntryLiveness {
    Live,
    BusyOrUnknown,
    PendingStartup,
    Dead,
}

#[derive(Debug, Clone)]
pub struct RegistryEntrySnapshot {
    pub entry: RegistryEntry,
    pub liveness: RegistryEntryLiveness,
    pub pid_live: bool,
}

impl RegistryEntrySnapshot {
    pub fn is_live_authority(&self) -> bool {
        matches!(
            self.liveness,
            RegistryEntryLiveness::Live | RegistryEntryLiveness::BusyOrUnknown
        )
    }

    pub fn is_pending_startup(&self) -> bool {
        self.liveness == RegistryEntryLiveness::PendingStartup
    }

    pub fn is_definitely_stale(&self) -> bool {
        self.liveness == RegistryEntryLiveness::Dead && !self.pid_live
    }

    pub fn is_uncertain(&self) -> bool {
        self.liveness == RegistryEntryLiveness::Dead && self.pid_live
    }
}

#[derive(Debug, Clone)]
pub struct RegistrySessionSnapshot {
    pub session_name: String,
    pub entries: Vec<RegistryEntrySnapshot>,
}

impl RegistrySessionSnapshot {
    pub fn authoritative_entry(&self) -> Option<&RegistryEntrySnapshot> {
        self.entries
            .iter()
            .rev()
            .find(|entry| entry.is_live_authority())
    }

    pub fn latest_entry(&self) -> Option<&RegistryEntrySnapshot> {
        self.entries.last()
    }

    pub fn stale_entries(&self) -> Vec<RegistryEntry> {
        let authoritative_session_id = self
            .authoritative_entry()
            .map(|entry| entry.entry.session_id.as_str());
        self.entries
            .iter()
            .filter(|entry| authoritative_session_id != Some(entry.entry.session_id.as_str()))
            .filter(|entry| entry.is_definitely_stale())
            .map(|entry| entry.entry.clone())
            .collect()
    }

    pub fn has_uncertain_entries(&self) -> bool {
        let authoritative_session_id = self
            .authoritative_entry()
            .map(|entry| entry.entry.session_id.as_str());
        self.entries
            .iter()
            .filter(|entry| authoritative_session_id != Some(entry.entry.session_id.as_str()))
            .any(RegistryEntrySnapshot::is_uncertain)
    }
}

#[derive(Debug, Clone, Default)]
pub struct RegistryAuthoritySnapshot {
    pub sessions: Vec<RegistrySessionSnapshot>,
}

impl RegistryAuthoritySnapshot {
    pub fn session(&self, session_name: &str) -> Option<&RegistrySessionSnapshot> {
        self.sessions
            .iter()
            .find(|session| session.session_name == session_name)
    }

    pub fn active_entries(&self) -> Vec<RegistryEntry> {
        let mut entries = self
            .sessions
            .iter()
            .filter_map(|session| {
                session
                    .authoritative_entry()
                    .map(|entry| entry.entry.clone())
            })
            .collect::<Vec<_>>();
        entries.sort_by(compare_registry_entry_created_at);
        entries
    }
}

pub fn authoritative_entry_by_session_name(
    home: &Path,
    session_name: &str,
) -> std::io::Result<Option<RegistryEntry>> {
    Ok(registry_authority_snapshot(home)?
        .session(session_name)
        .and_then(|session| {
            session
                .authoritative_entry()
                .map(|entry| entry.entry.clone())
        }))
}

pub fn latest_entry_by_session_name(
    home: &Path,
    session_name: &str,
) -> std::io::Result<Option<RegistryEntry>> {
    Ok(registry_authority_snapshot(home)?
        .session(session_name)
        .and_then(|session| session.latest_entry().map(|entry| entry.entry.clone())))
}

pub fn active_registry_entries(home: &Path) -> std::io::Result<Vec<RegistryEntry>> {
    Ok(registry_authority_snapshot(home)?.active_entries())
}

pub fn registry_authority_snapshot(home: &Path) -> std::io::Result<RegistryAuthoritySnapshot> {
    let data = read_registry(home)?;
    Ok(build_registry_authority_snapshot(home, &data))
}

fn build_registry_authority_snapshot(
    home: &Path,
    data: &RegistryData,
) -> RegistryAuthoritySnapshot {
    let mut sessions = BTreeMap::<String, Vec<RegistryEntrySnapshot>>::new();
    for entry in &data.sessions {
        let snapshot = registry_entry_snapshot_for_home(home, entry);
        sessions
            .entry(snapshot.entry.session_name.clone())
            .or_default()
            .push(snapshot);
    }

    let sessions = sessions
        .into_iter()
        .map(|(session_name, mut entries)| {
            entries.sort_by(compare_registry_entry_snapshot_created_at);
            RegistrySessionSnapshot {
                session_name,
                entries,
            }
        })
        .collect();
    RegistryAuthoritySnapshot { sessions }
}

pub(super) fn compare_registry_entry_created_at(
    left: &RegistryEntry,
    right: &RegistryEntry,
) -> Ordering {
    parsed_registry_created_at(&left.created_at)
        .expect("registry created_at should be validated before ordering")
        .cmp(
            &parsed_registry_created_at(&right.created_at)
                .expect("registry created_at should be validated before ordering"),
        )
        .then_with(|| left.session_id.cmp(&right.session_id))
}

fn compare_registry_entry_snapshot_created_at(
    left: &RegistryEntrySnapshot,
    right: &RegistryEntrySnapshot,
) -> Ordering {
    compare_registry_entry_created_at(&left.entry, &right.entry)
}

pub(super) fn parsed_registry_created_at(created_at: &str) -> std::io::Result<OffsetDateTime> {
    OffsetDateTime::parse(created_at, &Rfc3339).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid canonical RFC3339 timestamp '{created_at}': {error}"),
        )
    })
}
