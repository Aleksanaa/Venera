use std::collections::HashMap;
use venera_debug_protocol::ProcessIdentity;

const PROCESS_BUDGET: usize = 65536;
struct Instance {
    identity: ProcessIdentity,
    parent: u32,
    session: Option<u32>,
    ended: Option<u64>,
}
#[derive(Default)]
pub struct Processes {
    instances: HashMap<(u32, u64), Instance>,
    births: HashMap<u32, std::collections::BTreeSet<u64>>,
}
impl Processes {
    pub fn started(
        &mut self,
        identity: ProcessIdentity,
        parent: u32,
        session: Option<u32>,
    ) -> bool {
        let Some(start) = identity
            .start_time_filetime
            .as_ref()
            .and_then(|s| s.parse().ok())
        else {
            return false;
        };
        let key = (identity.pid, start);
        if self.instances.len() >= PROCESS_BUDGET && !self.instances.contains_key(&key) {
            return false;
        }
        self.births.entry(identity.pid).or_default().insert(start);
        self.instances
            .entry(key)
            .and_modify(|entry| {
                if identity.exe_path.is_some() {
                    entry.identity.exe_path.clone_from(&identity.exe_path);
                }
                if session.is_some() {
                    entry.session = session;
                }
            })
            .or_insert(Instance {
                identity,
                parent,
                session,
                ended: None,
            });
        true
    }
    pub fn ended(&mut self, pid: u32, start: u64, ended: u64) {
        if let Some(instance) = self.instances.get_mut(&(pid, start)) {
            instance.ended = Some(ended);
        }
    }
    pub fn observed(&self, pid: u32, at: u64) -> Option<(ProcessIdentity, Option<u32>)> {
        let entry = self
            .births
            .get(&pid)?
            .range(..=at)
            .rev()
            .find_map(|start| {
                self.instances
                    .get(&(pid, *start))
                    .filter(|entry| entry.ended.is_none_or(|end| at <= end))
            })?;
        Some((entry.identity.clone(), entry.session))
    }
    pub fn contains(&self, identity: &ProcessIdentity) -> bool {
        identity
            .start_time_filetime
            .as_ref()
            .and_then(|s| s.parse().ok())
            .is_some_and(|start| self.instances.contains_key(&(identity.pid, start)))
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
        let mut current = (identity.pid, start);
        for _ in 0..128 {
            let Some(entry) = self.instances.get(&current) else {
                break;
            };
            let parent_pid = entry.parent;
            let Some(parent) = self.observed(parent_pid, current.1) else {
                break;
            };
            let Some(birth) = parent
                .0
                .start_time_filetime
                .as_ref()
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|birth| *birth < current.1)
            else {
                break;
            };
            current = (parent_pid, birth);
            result.push(parent.0);
        }
        result
    }
    pub fn retire_before(&mut self, boundary: u64) {
        // Live ancestry keeps its exact historical parents across filter revisions.
        let mut keep = std::collections::HashSet::new();
        for entry in self
            .instances
            .values()
            .filter(|entry| entry.ended.is_none_or(|end| end >= boundary))
        {
            for parent in self.ancestors(&entry.identity, 0) {
                if let Some(start) = parent
                    .start_time_filetime
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    keep.insert((parent.pid, start));
                }
            }
        }
        self.instances.retain(|key, entry| {
            entry.ended.is_none_or(|end| end >= boundary) || keep.contains(key)
        });
        self.births.retain(|pid, births| {
            births.retain(|birth| self.instances.contains_key(&(*pid, *birth)));
            !births.is_empty()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn instance(pid: u32, start: u64) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            start_time_filetime: Some(start.to_string()),
            exe_path: None,
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
