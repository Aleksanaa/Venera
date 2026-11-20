use super::*;
use crate::ancestry::{process_bucket, Attribution, Lifecycle};
use std::{cell::RefCell, collections::VecDeque, sync::Arc};
use venera_debug_protocol::LogRecord;

const CACHE_BUCKETS: usize = 16;
const CACHE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Default)]
pub(super) struct ProcessCache {
    pages: VecDeque<((u64, u32), u64, Arc<Checkpoint>)>,
    pub(super) cold_reads: u64,
}

impl ProcessCache {
    fn get(&mut self, key: (u64, u32)) -> Option<Arc<Checkpoint>> {
        let index = self.pages.iter().position(|page| page.0 == key)?;
        let page = self.pages.remove(index).unwrap();
        let value = Arc::clone(&page.2);
        self.pages.push_front(page);
        Some(value)
    }

    fn put(&mut self, key: (u64, u32), bytes: u64, page: Arc<Checkpoint>) {
        self.pages.retain(|entry| entry.0 != key);
        self.pages.push_front((key, bytes, page));
        while self.pages.len() > CACHE_BUCKETS
            || self.pages.iter().map(|entry| entry.1).sum::<u64>() > CACHE_BYTES
        {
            self.pages.pop_back();
        }
    }

    pub(super) fn forget_boot(&mut self, boot: u64) {
        self.pages.retain(|entry| entry.0 .0 != boot);
    }
}

#[derive(Default)]
pub(super) struct Routes {
    parents: BTreeMap<u32, BTreeSet<u32>>,
    children: BTreeMap<u32, BTreeSet<u32>>,
    linked: BTreeSet<u32>,
    keyed: BTreeSet<u32>,
}

impl Routes {
    pub(super) fn set(&mut self, bucket: u32, page: &Checkpoint) {
        let parents = page.parent_buckets(bucket);
        if page.has_process_links() {
            self.linked.insert(bucket);
        } else {
            self.linked.remove(&bucket);
        }
        if page.has_keyed_parent_hints() {
            self.keyed.insert(bucket);
        } else {
            self.keyed.remove(&bucket);
        }
        if let Some(old) = self.parents.remove(&bucket) {
            for parent in old {
                let children = self.children.get_mut(&parent).unwrap();
                children.remove(&bucket);
                if children.is_empty() {
                    self.children.remove(&parent);
                }
            }
        }
        if !parents.is_empty() {
            for &parent in &parents {
                self.children.entry(parent).or_default().insert(bucket);
            }
            self.parents.insert(bucket, parents);
        }
    }

    fn closure(&self, buckets: &mut BTreeSet<u32>) {
        let mut pending: Vec<_> = buckets.iter().copied().collect();
        while let Some(bucket) = pending.pop() {
            for &related in self
                .parents
                .get(&bucket)
                .into_iter()
                .flatten()
                .chain(self.children.get(&bucket).into_iter().flatten())
            {
                if buckets.insert(related) {
                    pending.push(related);
                }
            }
        }
    }

    fn update_scope(&self, roots: &mut BTreeSet<u32>, explicit_key: bool) {
        let dependents: Vec<_> = roots
            .iter()
            .flat_map(|bucket| self.children.get(bucket).into_iter().flatten().copied())
            .collect();
        roots.extend(dependents);
        // A keyed hint may enrich its parent while resolving an invalidated link.
        // Inferred links only change the child, so sibling buckets of a read-only
        // parent do not participate in that update.
        if explicit_key || roots.iter().any(|bucket| self.keyed.contains(bucket)) {
            self.closure(roots);
        } else {
            let parents: Vec<_> = roots
                .iter()
                .flat_map(|bucket| self.parents.get(bucket).into_iter().flatten().copied())
                .collect();
            roots.extend(parents);
        }
    }
}

impl Journal {
    #[cfg(test)]
    pub(crate) fn process_cold_reads(&self) -> u64 {
        self.process_cache.borrow().cold_reads
    }
    pub(super) fn process_runtime_boot(&self, boot: &str) -> bool {
        self.process_runtime_boots.contains_key(boot)
    }

    pub(super) fn load_process_bucket(
        &self,
        boot: u64,
        bucket: u32,
    ) -> io::Result<Arc<Checkpoint>> {
        let key = (boot, bucket);
        if let Some(page) = self.process_cache.borrow_mut().get(key) {
            return Ok(page);
        }
        let boot_id = boot.to_string();
        let Some(parts) = self
            .metadata
            .process_segments
            .get(&boot_id)
            .and_then(|parts| parts.get(&bucket))
        else {
            return Ok(Arc::new(Checkpoint::default()));
        };
        let mut page = Checkpoint::default();
        let mut progress = bucket != 0;
        let mut bytes = 0;
        self.process_cache.borrow_mut().cold_reads += 1;
        for (part, &(size, alt)) in parts.iter().enumerate() {
            let file = File::open(self.process_path(&boot_id, bucket, part, alt))?;
            if file.metadata()?.len() != size {
                return Err(invalid("committed process extent differs from file"));
            }
            bytes += size;
            let mut reader = BufReader::new(file.take(size));
            let mut line = Vec::new();
            while reader.read_until(b'\n', &mut line)? != 0 {
                if !line.ends_with(b"\n") {
                    return Err(invalid("incomplete process checkpoint row"));
                }
                let row: CheckpointRow = serde_json::from_slice(&line)?;
                if matches!(row, CheckpointRow::Progress(..)) == progress {
                    return Err(invalid("unexpected process checkpoint progress row"));
                }
                page.restore_row(boot, bucket, row).map_err(invalid)?;
                progress = true;
                line.clear();
            }
        }
        if !progress {
            return Err(invalid("missing process checkpoint progress"));
        }
        let page = Arc::new(page);
        self.process_cache
            .borrow_mut()
            .put(key, bytes, Arc::clone(&page));
        Ok(page)
    }

    pub(super) fn warm_process_bucket(&self, boot: u64, bucket: u32, size: u64, page: Checkpoint) {
        self.process_cache
            .borrow_mut()
            .put((boot, bucket), size, Arc::new(page));
    }

    pub(super) fn load_process_buckets(
        &self,
        boot: u64,
        buckets: impl IntoIterator<Item = u32>,
    ) -> io::Result<Checkpoint> {
        let mut view = Checkpoint::default();
        for bucket in buckets {
            let page = self.load_process_bucket(boot, bucket)?;
            view.merge_bucket(&page, bucket);
        }
        Ok(view)
    }

    fn ensure_process_routes(&mut self, boot: u64) -> io::Result<()> {
        if !self.process_routes.contains_key(&boot) {
            let mut routes = Routes::default();
            if let Some(parts) = self.metadata.process_segments.get(&boot.to_string()) {
                for &bucket in parts.keys() {
                    let page = self.load_process_bucket(boot, bucket)?;
                    routes.set(bucket, &page);
                }
            }
            self.process_routes.insert(boot, routes);
        }
        Ok(())
    }

    pub(super) fn process_update_view(
        &mut self,
        boot: u64,
        changes: &[Lifecycle],
        interrupts: bool,
    ) -> io::Result<Checkpoint> {
        if changes.is_empty() && !interrupts {
            return self.load_process_buckets(boot, [0]);
        }
        self.ensure_process_routes(boot)?;
        let routes = &self.process_routes[&boot];
        let mut buckets = BTreeSet::new();
        let mut lookups = BTreeSet::new();
        let mut explicit_key = false;
        let mut all_links = interrupts;
        for change in changes {
            match change {
                Lifecycle::Start {
                    identity,
                    parent,
                    parent_key,
                    ..
                } => {
                    buckets.insert(process_bucket(identity.pid));
                    if *parent != 0 {
                        if parent_key.is_some() {
                            buckets.insert(process_bucket(*parent));
                            explicit_key = true;
                        } else {
                            lookups.insert(process_bucket(*parent));
                        }
                    }
                }
                Lifecycle::End { pid, .. } | Lifecycle::Context { pid, .. } => {
                    buckets.insert(process_bucket(*pid));
                }
                Lifecycle::Gap { .. } | Lifecycle::Discontinuity => all_links = true,
            }
        }
        if all_links {
            buckets.extend(routes.linked.iter().copied());
        }
        routes.update_scope(&mut buckets, explicit_key);
        buckets.extend(lookups);
        buckets.insert(0);
        self.load_process_buckets(boot, buckets)
    }

    pub(super) fn reclaim_process_bucket(
        &mut self,
        boot: u64,
        bucket: u32,
        now_ms: u64,
    ) -> io::Result<()> {
        self.ensure_process_routes(boot)?;
        if self.process_routes[&boot].children.contains_key(&bucket) {
            return Ok(());
        }
        let prior = self.load_process_bucket(boot, bucket)?;
        let mut next = (*prior).clone();
        let boot_id = boot.to_string();
        self.retire_process_history(&boot_id, &mut next, now_ms);
        if next != *prior {
            match self.commit_processes(
                self.metadata.clone(),
                Some((boot_id, next, Some((*prior).clone()))),
            ) {
                Err(error) if self.healthy && error.kind() == io::ErrorKind::InvalidInput => (),
                result => result?,
            }
        }
        Ok(())
    }

    pub(super) fn process_through(&self, boot: u64) -> io::Result<u64> {
        Ok(self.load_process_bucket(boot, 0)?.through_filetime)
    }

    pub(crate) fn attribute_process_record(
        &self,
        row: &mut LogRecord,
        boot: u64,
        at: u64,
        wait: bool,
    ) -> io::Result<Attribution> {
        self.check()?;
        row.ancestors.clear();
        if row.component == "kernel" || row.source == "wpp_recorder" {
            row.process = None;
            return Ok(Attribution::Ready);
        }
        let Some(observed) = &row.process else {
            return Ok(Attribution::Ready);
        };
        let header = self.load_process_bucket(boot, 0)?;
        if wait && at > header.through_filetime {
            return Ok(Attribution::Pending);
        }
        let bucket = self.load_process_bucket(boot, process_bucket(observed.pid))?;
        let Some((identity, mut current)) =
            bucket
                .processes
                .attributed_instance(&header.processes, observed, boot, at)
        else {
            return Ok(Attribution::Missing(observed.pid));
        };
        let key = |value: &venera_debug_protocol::ProcessIdentity| {
            (
                value.pid,
                value.start_time_filetime.clone(),
                value.start_key.clone(),
            )
        };
        let mut seen = BTreeSet::from([key(&current)]);
        loop {
            let bucket = self.load_process_bucket(boot, process_bucket(current.pid))?;
            let Some((parent, frozen)) = bucket.processes.parent_step(&header.processes, &current)
            else {
                break;
            };
            if !seen.insert(key(&parent)) {
                break;
            }
            row.ancestors.push(parent.clone());
            if frozen {
                break;
            }
            current = parent;
        }
        row.process = Some(identity);
        Ok(Attribution::Ready)
    }
}
