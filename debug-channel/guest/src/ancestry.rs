use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use venera_debug_protocol::ProcessIdentity;

const PROCESS_BUDGET: usize = 65536;
#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Instance {
    identity: ProcessIdentity,
    parent: Option<ProcessIdentity>,
    session: Option<u32>,
    ended: Option<u64>,
    #[serde(default = "started_default", skip_serializing_if = "is_true")]
    started: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_hint: Option<(u32, Option<u64>)>,
    #[serde(default, skip_serializing_if = "is_false")]
    parent_frozen: bool,
}
fn is_true(value: &bool) -> bool {
    *value
}
fn is_false(value: &bool) -> bool {
    !*value
}
fn started_default() -> bool {
    true
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Interruption {
    from: u64,
    through: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum StoredProcesses {
    Current {
        instances: Vec<Instance>,
        interruptions: Vec<Interruption>,
        retired_before: u64,
    },
    Legacy(Vec<Instance>),
}
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "StoredProcesses", into = "StoredProcesses")]
pub struct Processes {
    instances: BTreeMap<(u32, u64, Option<u64>), Instance>,
    interruptions: Vec<Interruption>,
    retired_before: u64,
}

impl From<Processes> for StoredProcesses {
    fn from(value: Processes) -> Self {
        let mut entries: Vec<_> = value.instances.into_iter().collect();
        entries.sort_by_key(|(key, _)| *key);
        Self::Current {
            instances: entries.into_iter().map(|(_, entry)| entry).collect(),
            interruptions: value.interruptions,
            retired_before: value.retired_before,
        }
    }
}

impl TryFrom<StoredProcesses> for Processes {
    type Error = &'static str;
    fn try_from(value: StoredProcesses) -> Result<Self, Self::Error> {
        let (entries, interruptions, retired_before) = match value {
            StoredProcesses::Current {
                instances,
                interruptions,
                retired_before,
            } => (instances, interruptions, retired_before),
            StoredProcesses::Legacy(entries) => (entries, Vec::new(), 0),
        };
        if interruptions.len() > 16 || interruptions.iter().any(|gap| gap.through < gap.from) {
            return Err("invalid process interruption intervals");
        }
        if entries.len() > PROCESS_BUDGET {
            return Err("process checkpoint exceeds capacity");
        }
        let mut result = Self {
            interruptions,
            retired_before,
            ..Self::default()
        };
        for entry in entries {
            let start = entry
                .identity
                .start_time_filetime
                .as_ref()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or("process checkpoint has invalid creation time")?;
            let pid = entry.identity.pid;
            let key = entry
                .identity
                .start_key
                .as_ref()
                .map(|value| value.parse::<u64>())
                .transpose()
                .map_err(|_| "invalid process start key")?;
            if result.instances.insert((pid, start, key), entry).is_some() {
                return Err("duplicate process checkpoint identity");
            }
        }
        Ok(result)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Lifecycle {
    Start {
        identity: ProcessIdentity,
        parent: u32,
        parent_key: Option<u64>,
        session: Option<u32>,
    },
    End {
        pid: u32,
        start: u64,
        end: u64,
        key: Option<u64>,
    },
    Context {
        pid: u32,
        key: u64,
        time: u64,
    },
    Discontinuity,
    Gap {
        from: u64,
        through: u64,
    },
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub processes: Processes,
    pub through_filetime: u64,
}

impl Checkpoint {
    pub fn attribute_record(
        &self,
        record: &mut venera_debug_protocol::LogRecord,
        boot: u64,
        time: u64,
        wait: bool,
    ) -> bool {
        record.ancestors.clear();
        if record.component == "kernel" || record.source == "wpp_recorder" {
            record.process = None;
            return true;
        }
        if let Some(observed) = &record.process {
            if wait && time > self.through_filetime {
                return false;
            }
            if let Some((identity, ancestors)) = self.processes.attribute(observed, boot, time) {
                record.process = Some(identity);
                record.ancestors = ancestors;
            }
        }
        true
    }
}

impl Processes {
    pub fn interrupt(&mut self, from: u64, through: u64) {
        self.interruptions.push(Interruption {
            from,
            through: through.max(from),
        });
        self.interruptions.sort_by_key(|gap| gap.from);
        let mut merged: Vec<Interruption> = Vec::new();
        for gap in self.interruptions.drain(..) {
            if let Some(previous) = merged
                .last_mut()
                .filter(|previous| previous.through >= gap.from)
            {
                previous.through = previous.through.max(gap.through);
            } else {
                merged.push(gap);
            }
        }
        while merged.len() > 16 {
            let oldest = merged.remove(0);
            self.retired_before = self.retired_before.max(oldest.through);
        }
        self.interruptions = merged;
        self.resolve_parents();
    }

    pub fn resume(&mut self, at: u64) {
        for gap in &mut self.interruptions {
            if gap.through == u64::MAX {
                gap.through = at.max(gap.from);
            }
        }
        self.resolve_parents();
    }

    fn valid_interval(&self, start: u64, at: u64) -> bool {
        start >= self.retired_before
            && !self
                .interruptions
                .iter()
                .any(|gap| start < gap.through && at >= gap.from)
    }

    pub fn has_history(&self) -> bool {
        !self.instances.is_empty() || !self.interruptions.is_empty()
    }
    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    pub fn attribute(
        &self,
        observed: &ProcessIdentity,
        boot: u64,
        at: u64,
    ) -> Option<(ProcessIdentity, Vec<ProcessIdentity>)> {
        if observed
            .boot_time_filetime
            .as_deref()
            .is_some_and(|value| value != boot.to_string())
        {
            return None;
        }
        let key = observed
            .start_key
            .as_ref()
            .map(|value| value.parse::<u64>())
            .transpose()
            .ok()?;
        let (_, entry) = self.observed_entry(observed.pid, at, key)?;
        let mut identity = entry.identity.clone();
        if identity.boot_time_filetime.as_deref() != Some(&boot.to_string()) {
            return None;
        }
        if observed
            .start_time_filetime
            .as_ref()
            .is_some_and(|start| Some(start) != identity.start_time_filetime.as_ref())
        {
            return None;
        }
        if let Some(key) = &observed.start_key {
            if identity
                .start_key
                .as_ref()
                .is_some_and(|known| known != key)
            {
                return None;
            }
            identity.start_key = Some(key.clone());
        }
        let ancestors = self.ancestors(&entry.identity, 0);
        Some((identity, ancestors))
    }
    pub fn apply(&mut self, change: Lifecycle) -> bool {
        match change {
            Lifecycle::Start {
                identity,
                parent,
                parent_key,
                session,
            } => self.started_bound(identity, parent, parent_key, session),
            Lifecycle::End {
                pid,
                start,
                end,
                key,
            } => {
                let applied = self.ended_key(pid, start, end, key);
                self.resolve_parents();
                applied
            }
            Lifecycle::Context { pid, key, time } => {
                if let Some((index, _)) = self.observed_entry(pid, time, Some(key)) {
                    let index = *index;
                    let mut entry = self.instances.remove(&index).unwrap();
                    self.enrich_parent_references(&entry.identity, key);
                    entry.identity.start_key = Some(key.to_string());
                    self.instances.insert((pid, index.1, Some(key)), entry);
                }
                true
            }
            Lifecycle::Discontinuity => {
                self.interrupt(0, u64::MAX);
                true
            }
            Lifecycle::Gap { from, through } => {
                self.interrupt(from, through);
                true
            }
        }
    }

    pub fn evict_oldest(&mut self) -> bool {
        let key = self
            .instances
            .keys()
            .min_by_key(|(pid, start, key)| (*start, *pid, *key))
            .copied();
        if let Some(key) = key {
            let entry = self.instances.remove(&key).unwrap();
            for child in self.instances.values_mut() {
                if child.parent.as_ref().is_some_and(|parent| {
                    parent.pid == entry.identity.pid
                        && parent.start_time_filetime == entry.identity.start_time_filetime
                        && parent.boot_time_filetime == entry.identity.boot_time_filetime
                        && parent.start_key == entry.identity.start_key
                }) {
                    child.parent_frozen = true;
                }
            }
            if !entry.started {
                self.retired_before = self.retired_before.max(key.1.saturating_add(1));
            }
            true
        } else if !self.interruptions.is_empty() && self.interruptions[0].through != u64::MAX {
            let gap = self.interruptions.remove(0);
            self.retired_before = self.retired_before.max(gap.through);
            true
        } else {
            false
        }
    }

    pub fn started(
        &mut self,
        identity: ProcessIdentity,
        parent: u32,
        session: Option<u32>,
    ) -> bool {
        self.started_bound(identity, parent, None, session)
    }

    fn started_bound(
        &mut self,
        identity: ProcessIdentity,
        parent: u32,
        parent_key: Option<u64>,
        session: Option<u32>,
    ) -> bool {
        let Some(start) = identity
            .start_time_filetime
            .as_ref()
            .and_then(|s| s.parse().ok())
        else {
            return false;
        };
        let Ok(start_key) = identity
            .start_key
            .as_ref()
            .map(|value| value.parse::<u64>())
            .transpose()
        else {
            return false;
        };
        if start < self.retired_before {
            return true;
        }
        let existing = self.instance_key(&identity);
        let key = if start_key.is_some() {
            (identity.pid, start, start_key)
        } else {
            existing.unwrap_or((identity.pid, start, None))
        };
        if let Some(existing) = existing.filter(|existing| *existing != key) {
            let mut entry = self.instances.remove(&existing).unwrap();
            self.enrich_parent_references(&entry.identity, start_key.unwrap());
            entry.identity.start_key.clone_from(&identity.start_key);
            self.instances.insert(key, entry);
        }
        if self.instances.len() >= PROCESS_BUDGET && !self.instances.contains_key(&key) {
            return false;
        }
        let pid = identity.pid;
        let previous = self
            .instances
            .range((pid, 0, None)..(pid, start, None))
            .next_back()
            .map(|(key, _)| key.1);
        if let Some(previous) = previous {
            for (_, entry) in self
                .instances
                .range_mut((pid, previous, None)..=(pid, previous, Some(u64::MAX)))
            {
                entry.ended = Some(entry.ended.unwrap_or(start).min(start));
            }
        }
        let next = self
            .instances
            .range((
                std::ops::Bound::Excluded((pid, start, Some(u64::MAX))),
                std::ops::Bound::Included((pid, u64::MAX, Some(u64::MAX))),
            ))
            .next()
            .map(|(key, _)| key.1);
        if let Some(key) = parent_key {
            self.apply(Lifecycle::Context {
                pid: parent,
                key,
                time: start,
            });
        }
        let parent_hint = (parent != 0 && parent != pid).then_some((parent, parent_key));
        let parent = self
            .observed_entry(parent, start, parent_key)
            .map(|(_, entry)| entry.identity.clone())
            .filter(|parent| {
                parent.pid != pid
                    && parent.boot_time_filetime == identity.boot_time_filetime
                    && parent
                        .start_time_filetime
                        .as_ref()
                        .and_then(|v| v.parse::<u64>().ok())
                        .is_some_and(|birth| birth <= start)
            });
        self.instances
            .entry(key)
            .and_modify(|entry| {
                if entry.parent.is_none() {
                    entry.parent = parent.clone();
                }
                entry.started = true;
                if entry.parent_hint.is_none() {
                    entry.parent_hint = parent_hint;
                }
                entry
                    .identity
                    .boot_time_filetime
                    .clone_from(&identity.boot_time_filetime);
                if identity.exe_path.is_some() {
                    entry.identity.exe_path.clone_from(&identity.exe_path);
                }
                if session.is_some() {
                    entry.session = session;
                }
                if entry.identity.start_key.is_none() {
                    entry.identity.start_key.clone_from(&identity.start_key);
                }
            })
            .or_insert(Instance {
                identity,
                parent,
                session,
                ended: next,
                started: true,
                parent_hint,
                parent_frozen: false,
            });
        self.resolve_parents();
        true
    }
    pub fn ended(&mut self, pid: u32, start: u64, ended: u64) {
        self.ended_key(pid, start, ended, None);
        self.resolve_parents();
    }

    fn ended_key(&mut self, pid: u32, start: u64, ended: u64, key: Option<u64>) -> bool {
        if start < self.retired_before {
            return true;
        }
        let identity = ProcessIdentity {
            pid,
            start_time_filetime: Some(start.to_string()),
            start_key: key.map(|key| key.to_string()),
            ..Default::default()
        };
        if let Some(index) = self.instance_key(&identity) {
            let mut instance = self.instances.remove(&index).unwrap();
            instance.ended = Some(instance.ended.unwrap_or(ended).min(ended));
            if let Some(key) = key {
                self.enrich_parent_references(&instance.identity, key);
                instance.identity.start_key = Some(key.to_string());
            }
            self.instances
                .insert((pid, start, key.or(index.2)), instance);
            return true;
        }
        if self.instances.len() < PROCESS_BUDGET {
            self.instances.insert(
                (pid, start, key),
                Instance {
                    identity: ProcessIdentity {
                        pid,
                        start_time_filetime: Some(start.to_string()),
                        start_key: key.map(|key| key.to_string()),
                        ..Default::default()
                    },
                    parent: None,
                    parent_hint: None,
                    session: None,
                    ended: Some(ended),
                    started: false,
                    parent_frozen: false,
                },
            );
            true
        } else {
            false
        }
    }

    fn enrich_parent_references(&mut self, identity: &ProcessIdentity, key: u64) {
        for entry in self.instances.values_mut() {
            if entry.parent_frozen {
                continue;
            }
            if let Some(parent) = &mut entry.parent {
                if parent.pid == identity.pid
                    && parent.start_time_filetime == identity.start_time_filetime
                    && parent.boot_time_filetime == identity.boot_time_filetime
                    && parent.start_key == identity.start_key
                {
                    parent.start_key = Some(key.to_string());
                }
            }
        }
    }

    fn resolve_parents(&mut self) {
        let invalid: Vec<_> = self
            .instances
            .iter()
            .filter_map(|(index, entry)| {
                if entry.parent_frozen {
                    return None;
                }
                let parent = entry.parent.as_ref()?;
                let parent_index = self.exact_key(parent)?;
                let parent_entry = self.instances.get(&parent_index)?;
                (!self.valid_interval(parent_index.1, index.1)
                    || parent_entry.ended.is_some_and(|end| end <= index.1))
                .then_some(*index)
            })
            .collect();
        for index in invalid {
            self.instances.get_mut(&index).unwrap().parent = None;
        }
        let hints: Vec<_> = self
            .instances
            .iter()
            .filter_map(|(index, entry)| {
                let (pid, Some(key)) = entry.parent_hint? else {
                    return None;
                };
                (entry.started && entry.parent.is_none()).then_some((pid, key, index.1))
            })
            .collect();
        for (pid, key, time) in hints {
            self.apply(Lifecycle::Context { pid, key, time });
        }
        let resolved: Vec<_> = self
            .instances
            .iter()
            .filter_map(|(index, entry)| {
                if !entry.started || entry.parent.is_some() {
                    return None;
                }
                let (pid, key) = entry.parent_hint?;
                let (_, parent) = self.observed_entry(pid, index.1, key)?;
                if parent.identity.boot_time_filetime != entry.identity.boot_time_filetime {
                    return None;
                }
                Some((*index, parent.identity.clone()))
            })
            .collect();
        for (index, parent) in resolved {
            self.instances.get_mut(&index).unwrap().parent = Some(parent);
        }
    }

    fn exact_key(&self, identity: &ProcessIdentity) -> Option<(u32, u64, Option<u64>)> {
        let key = (
            identity.pid,
            identity.start_time_filetime.as_ref()?.parse().ok()?,
            identity
                .start_key
                .as_ref()
                .map(|key| key.parse::<u64>())
                .transpose()
                .ok()?,
        );
        self.instances
            .get(&key)
            .filter(|entry| entry.identity.boot_time_filetime == identity.boot_time_filetime)
            .map(|_| key)
    }

    fn instance_key(&self, identity: &ProcessIdentity) -> Option<(u32, u64, Option<u64>)> {
        let start = identity.start_time_filetime.as_ref()?.parse::<u64>().ok()?;
        let key = identity
            .start_key
            .as_ref()
            .map(|value| value.parse::<u64>())
            .transpose()
            .ok()?;
        if key.is_some() && self.instances.contains_key(&(identity.pid, start, key)) {
            return Some((identity.pid, start, key));
        }
        let mut entries = self
            .instances
            .range((identity.pid, start, None)..=(identity.pid, start, Some(u64::MAX)));
        let (index, _) = entries.next()?;
        if entries.next().is_some() || (key.is_some() && index.2.is_some()) {
            return None;
        }
        Some(*index)
    }

    fn observed_entry(
        &self,
        pid: u32,
        at: u64,
        key: Option<u64>,
    ) -> Option<(&(u32, u64, Option<u64>), &Instance)> {
        let range = || {
            self.instances
                .range((pid, 0, None)..=(pid, at, Some(u64::MAX)))
        };
        let valid = |index: &(u32, u64, Option<u64>), entry: &Instance| {
            entry.started
                && self.valid_interval(index.1, at)
                && entry.ended.is_none_or(|end| at < end)
        };
        if let Some(key) = key {
            if let Some(entry) = range().rev().find(|(index, _)| index.2 == Some(key)) {
                return valid(entry.0, entry.1).then_some(entry);
            }
        }
        let mut entries = range().rev();
        let entry = entries.next()?;
        if entries
            .next()
            .is_some_and(|(previous, _)| previous.1 == entry.0 .1)
        {
            return None;
        }
        if key.is_some() && entry.0 .2.is_some() {
            return None;
        }
        valid(entry.0, entry.1).then_some(entry)
    }

    pub fn observed(&self, pid: u32, at: u64) -> Option<(ProcessIdentity, Option<u32>)> {
        let (_, entry) = self.observed_entry(pid, at, None)?;
        Some((entry.identity.clone(), entry.session))
    }
    pub fn contains(&self, identity: &ProcessIdentity) -> bool {
        self.instance_key(identity).is_some()
    }
    pub fn ancestors(
        &self,
        identity: &ProcessIdentity,
        filter_started: u64,
    ) -> Vec<ProcessIdentity> {
        let Some(start) = identity
            .start_time_filetime
            .as_ref()
            .and_then(|s| s.parse::<u64>().ok())
        else {
            return Vec::new();
        };
        if start < filter_started {
            return Vec::new();
        }
        let mut result = Vec::new();
        let mut current = identity.clone();
        for _ in 0..128 {
            let Some(entry) = self
                .exact_key(&current)
                .and_then(|key| self.instances.get(&key))
            else {
                break;
            };
            let Some(parent) = &entry.parent else {
                break;
            };
            let Some(birth) = parent
                .start_time_filetime
                .as_ref()
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|birth| {
                    parent.pid != current.pid
                        && *birth
                            <= current
                                .start_time_filetime
                                .as_ref()
                                .unwrap()
                                .parse::<u64>()
                                .unwrap()
                })
            else {
                break;
            };
            if entry.parent_hint.and_then(|(_, key)| key).is_none()
                && !self.valid_interval(
                    birth,
                    current
                        .start_time_filetime
                        .as_ref()
                        .unwrap()
                        .parse()
                        .unwrap(),
                )
            {
                break;
            }
            current = parent.clone();
            result.push(current.clone());
            if entry.parent_frozen {
                break;
            }
        }
        result
    }
    pub fn retire_before(&mut self, boundary: u64) {
        self.retire_history(0, boundary);
    }

    pub fn retire_history(&mut self, startup_before: u64, boundary: u64) {
        let retained = |entry: &Instance| {
            entry.ended.is_none_or(|end| end >= boundary)
                || entry
                    .identity
                    .start_time_filetime
                    .as_ref()
                    .and_then(|value| value.parse::<u64>().ok())
                    .is_some_and(|start| start < startup_before)
        };
        // Live ancestry keeps its exact historical parents across filter revisions.
        let mut keep = std::collections::HashSet::new();
        for entry in self.instances.values().filter(|entry| retained(entry)) {
            for parent in self.ancestors(&entry.identity, 0) {
                if let Some(key) = self.exact_key(&parent) {
                    keep.insert(key);
                }
            }
        }
        self.instances
            .retain(|key, entry| retained(entry) || keep.contains(key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_gap_invalidates_frozen_parent_interval() {
        let identity = |pid: u32, start: u64, key: u64| ProcessIdentity {
            pid,
            start_time_filetime: Some(start.to_string()),
            exe_path: Some(format!("{pid}.exe")),
            boot_time_filetime: Some("100".into()),
            start_key: Some(key.to_string()),
        };
        let mut processes = Processes::default();
        processes.started(identity(10, 110, 11), 0, None);
        processes.started(identity(20, 130, 22), 10, None);
        assert!(processes.evict_oldest());
        assert_eq!(
            processes.ancestors(&identity(20, 130, 22), 0),
            [identity(10, 110, 11)]
        );
        processes.apply(Lifecycle::Gap {
            from: 120,
            through: 125,
        });
        let processes: Processes =
            serde_json::from_slice(&serde_json::to_vec(&processes).unwrap()).unwrap();
        assert_eq!(
            processes.observed(20, 140).unwrap().0,
            identity(20, 130, 22)
        );
        assert_eq!(processes.ancestors(&identity(20, 130, 22), 0), []);
    }

    fn instance(pid: u32, start: u64) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            start_time_filetime: Some(start.to_string()),
            exe_path: None,
            ..Default::default()
        }
    }
    #[test]
    fn short_lived_parent_and_pid_reuse_keep_exact_descendants() {
        let mut processes = Processes::default();
        processes.started(instance(10, 100), 0, Some(1));
        processes.started(instance(20, 200), 10, Some(1));
        processes.started(instance(30, 300), 20, Some(1));
        processes.ended(20, 200, 310);
        processes.started(instance(20, 400), 99, Some(2));
        assert_eq!(
            processes.ancestors(&instance(30, 300), 150),
            vec![instance(20, 200), instance(10, 100)]
        );
        assert_eq!(
            processes.ancestors(&instance(30, 300), 250),
            vec![instance(20, 200), instance(10, 100)]
        );
        assert_eq!(processes.ancestors(&instance(30, 300), 350), Vec::new());
        assert_eq!(
            processes.observed(20, 250),
            Some((instance(20, 200), Some(1)))
        );
        assert_eq!(
            processes.observed(20, 450),
            Some((instance(20, 400), Some(2)))
        );
        assert!(processes.contains(&instance(30, 300)));
        processes.retire_before(350);
        assert_eq!(
            processes.ancestors(&instance(30, 300), 150),
            vec![instance(20, 200), instance(10, 100)]
        );
    }
}
