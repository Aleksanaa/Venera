use super::*;
use crate::engine_events::{GraphicsUpdate, MAX_GRAPHICS_BYTES};

impl Journal {
    fn graphics_path(&self, boot: u64, alternate: bool) -> PathBuf {
        self.root
            .join(format!("graphics-{boot}-{}.frag", u8::from(alternate)))
    }

    pub fn graphics_fragments(&self, boot: u64) -> io::Result<Vec<u8>> {
        self.check()?;
        if let Some(&(length, alternate)) = self.metadata.graphics_fragments.get(&boot) {
            let mut file = File::open(self.graphics_path(boot, alternate))?;
            if length == 0 || length > MAX_GRAPHICS_BYTES as u64 || file.metadata()?.len() != length
            {
                return Err(invalid("invalid graphics fragment extent"));
            }
            let mut bytes = vec![0; length as usize];
            file.read_exact(&mut bytes)?;
            return Ok(bytes);
        }
        Ok(Vec::new())
    }

    pub fn graphics_boots(&self) -> io::Result<Vec<u64>> {
        self.check()?;
        Ok(self.metadata.graphics_fragments.keys().copied().collect())
    }

    pub fn finish_graphics_boot(&mut self, boot: u64, now_ms: u64) -> io::Result<()> {
        let bytes = self.graphics_fragments(boot)?;
        if bytes.is_empty() {
            return Ok(());
        }
        let count = crate::engine_events::Decoder::restore(boot, &bytes)?.incomplete();
        let event = HistoryEvent::Gap {
            gap: venera_debug_protocol::v1::Gap {
                stream_id: self.metadata.journal_id.clone(),
                reason: "graphics_fragments_incomplete".into(),
                count: Some(DecimalU64(count)),
                first_seq: None,
                last_seq: None,
            },
        };
        self.append_stored(
            vec![(
                JournalInput {
                    boot_id: boot.to_string(),
                    recorded_at_ms: now_ms,
                    startup: false,
                    event,
                },
                None,
            )],
            None,
            Some(GraphicsUpdate {
                boot,
                bytes: Vec::new(),
            }),
        )?;
        Ok(())
    }

    pub(super) fn recover_graphics(&self) -> io::Result<()> {
        let mut live = BTreeSet::new();
        for (&boot, &(length, alternate)) in &self.metadata.graphics_fragments {
            let path = self.graphics_path(boot, alternate);
            if boot == 0
                || length == 0
                || length > MAX_GRAPHICS_BYTES as u64
                || fs::metadata(&path)?.len() != length
            {
                return Err(invalid("invalid committed graphics fragment extent"));
            }
            live.insert(path);
        }
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_file()
                && entry.file_name().to_string_lossy().starts_with("graphics-")
                && path.extension().is_some_and(|ext| ext == "frag")
                && !live.contains(&path)
            {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    pub(super) fn publish_graphics(
        &mut self,
        mut metadata: Metadata,
        update: GraphicsUpdate,
        diagnostic: Option<&HistoryRecord>,
        encoded: &[u8],
    ) -> io::Result<()> {
        let GraphicsUpdate { boot, bytes } = update;
        let length = bytes.len() as u64;
        let old = self.metadata.graphics_fragments.get(&boot).copied();
        if boot == 0 || length > MAX_GRAPHICS_BYTES as u64 {
            return Err(request_error(
                "graphics fragment checkpoint exceeds capacity",
            ));
        }
        let extent = if length == 0 {
            None
        } else if old.is_some_and(|(size, _)| size == length)
            && self.graphics_fragments(boot)? == bytes
        {
            old
        } else {
            Some((length, old.is_some_and(|(_, slot)| !slot)))
        };
        match extent {
            Some(value) => metadata.graphics_fragments.insert(boot, value),
            None => metadata.graphics_fragments.remove(&boot),
        };
        if metadata == self.metadata {
            return Ok(());
        }
        let mut retired = None;
        if let Some((_, slot)) = old.filter(|old| Some(*old) != extent) {
            let path = self.graphics_path(boot, slot);
            let charge = space::charge(self.unit, &File::open(&path)?)?;
            retired = Some((path, charge));
        }
        let staged = extent.filter(|extent| Some(*extent) != old);
        let fragment_charge = staged.map_or(0, |(len, _)| space::rounded(self.unit, len));
        // A completion carries at most one diagnostic. Give it a fresh segment so
        // both staged files can be discarded independently on reopen.
        let diagnostic_len = encoded.len() as u64;
        let position = diagnostic.map(|record| record.cursor.position.0);
        let mut segments = self
            .segments
            .iter()
            .map(|(&key, segment)| (key, (segment.bytes, segment.alternate)))
            .collect::<BTreeMap<_, _>>();
        if let Some(position) = position {
            segments.insert(position, (diagnostic_len, false));
        }
        let data_charge = fragment_charge + space::rounded(self.unit, diagnostic_len);
        metadata.segments = Some(segments);
        let manifest = self.metadata_reserve(serde_json::to_vec(&metadata)?.len() as u64)?;
        // Keep the ordinary append runway in the resulting generation. Retired
        // files still occupy physical space until the manifest is published.
        let runway = (self.policy.max_bytes / 4).min(self.prepare_limit());
        let victims = self.evictions(
            (data_charge + manifest + runway).saturating_sub(
                self.metadata_size + retired.as_ref().map_or(0, |(_, charge)| *charge),
            ),
            self.metadata.high_water,
        )?;
        for key in &victims {
            metadata.segments.as_mut().unwrap().remove(key);
        }
        let manifest = self.metadata_reserve(serde_json::to_vec(&metadata)?.len() as u64)?;
        self.graphics_prepare = fragment_charge;
        if let Err(error) = self.require_space(data_charge + manifest) {
            self.graphics_prepare = 0;
            return Err(error);
        }

        let result = (|| {
            if let Some((_, slot)) = staged {
                self.stage_file(&self.graphics_path(boot, slot), &bytes)?;
            }
            if let Some(record) = diagnostic {
                let key = record.cursor.position.0;
                let charge = self.stage_file(&self.data_path(key, false), encoded)?;
                self.insert_segment(
                    key,
                    Segment {
                        alternate: false,
                        charge,
                        entries: vec![Entry {
                            position: key,
                            offset: 0,
                            bytes: diagnostic_len,
                            time: record.recorded_at_ms.0,
                            identity: None,
                            attribution_time: None,
                            revision_target: None,
                        }],
                        boot: record.boot_id.clone(),
                        startup: record.startup,
                        bytes: diagnostic_len,
                    },
                );
            }
            for key in victims {
                self.remove_segment(key)?;
            }
            self.retired.extend(retired);
            self.boots_dirty |= self.metadata.recent_boots != metadata.recent_boots;
            self.metadata = metadata;
            self.persist_metadata()
        })();
        self.graphics_prepare = 0;
        if result.is_err() {
            self.healthy = false;
        }
        result
    }
}
