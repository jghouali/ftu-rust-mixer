use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TrySendError};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use alsa::{card::Iter as CardIter, ctl::ElemType, hctl::HCtl, Ctl};
use alsa_sys as alsa_ffi;
use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;

use crate::models::{ControlDescriptor, ControlKind, RouteRef, RoutingIndex};

#[derive(Debug, Clone)]
pub struct CardInfo {
    pub index: u32,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Alsa,
}

pub struct AlsaBackend {
    pub card_index: u32,
    pub card_label: String,
    ctl_handle: Option<Ctl>,
    hctl_handle: Option<HCtl>,
    kind_cache_by_numid: Mutex<HashMap<u32, ControlKind>>,
}

impl AlsaBackend {
    pub fn detect_cards() -> Result<Vec<CardInfo>> {
        let mut cards = Vec::new();
        for card in CardIter::new() {
            let card = card.context("Failed to enumerate ALSA cards")?;
            let idx = card.get_index();
            if idx < 0 {
                continue;
            }
            let name = card.get_name().unwrap_or_else(|_| "Unknown".to_string());
            cards.push(CardInfo {
                index: idx as u32,
                name,
            });
        }
        Ok(cards)
    }

    pub fn pick_card(card_override: Option<u32>) -> Result<Self> {
        let cards = Self::detect_cards()?;
        if cards.is_empty() {
            bail!("No ALSA cards detected");
        }

        let card = if let Some(idx) = card_override {
            cards
                .iter()
                .find(|c| c.index == idx)
                .cloned()
                .ok_or_else(|| anyhow!("Requested card index {idx} not found"))?
        } else {
            cards
                .iter()
                .find(|c| {
                    let l = c.name.to_lowercase();
                    l.contains("ultra") || l.contains("f8r") || l.contains("fast track")
                })
                .cloned()
                .or_else(|| cards.first().cloned())
                .ok_or_else(|| anyhow!("No ALSA cards found"))?
        };

        let hctl = Self::open_hctl_handle(card.index)?;
        let ctl = Self::open_ctl_handle(card.index)?;
        Ok(Self {
            card_index: card.index,
            card_label: card.name,
            ctl_handle: Some(ctl),
            hctl_handle: Some(hctl),
            kind_cache_by_numid: Mutex::new(HashMap::new()),
        })
    }

    pub fn active_backend(&self) -> BackendKind {
        BackendKind::Alsa
    }

    pub fn start_event_listener<F>(&self, mut notify_ui: F) -> Option<Receiver<()>>
    where
        F: FnMut() + Send + 'static,
    {
        let card_index = self.card_index;
        let (tx, rx) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let Ok(hctl) = Self::open_hctl_handle(card_index) else {
                return;
            };
            let mut last_notified = Instant::now() - Duration::from_secs(1);
            const MIN_NOTIFY_INTERVAL: Duration = Duration::from_millis(70);

            loop {
                match hctl.wait(Some(1000)) {
                    Ok(true) => {
                        let handled = hctl.handle_events().unwrap_or(0);
                        if handled == 0 {
                            continue;
                        }
                        if last_notified.elapsed() < MIN_NOTIFY_INTERVAL {
                            continue;
                        }
                        match tx.try_send(()) {
                            Ok(()) => {
                                last_notified = Instant::now();
                                notify_ui();
                            }
                            Err(TrySendError::Full(_)) => {}
                            Err(TrySendError::Disconnected(_)) => break,
                        }
                    }
                    Ok(false) => {}
                    Err(_) => break,
                }
            }
        });
        Some(rx)
    }

    pub fn list_controls(&self) -> Result<Vec<ControlDescriptor>> {
        let ctl = self
            .ctl_handle
            .as_ref()
            .ok_or_else(|| anyhow!("Native ALSA ctl not initialized"))?;
        let hctl = self
            .hctl_handle
            .as_ref()
            .ok_or_else(|| anyhow!("Native ALSA backend not initialized"))?;
        let mut controls = Vec::new();
        for elem in hctl.elem_iter() {
            let id = elem.get_id()?;
            let info = elem.info()?;
            let name = id
                .get_name()
                .map(str::to_string)
                .unwrap_or_else(|_| format!("numid={}", id.get_numid()));
            let kind = Self::infer_control_kind_from_elem(&id, &info, ctl)?;
            let channels = Self::channels_from_kind(&kind);
            let mut values = self.read_values_from_elem_for_kind(&elem, &kind)?;
            if values.is_empty() {
                values = vec!["0".to_string(); channels];
            }
            let mut ctrl = ControlDescriptor {
                numid: id.get_numid(),
                name,
                iface: format!("{:?}", id.get_interface()),
                index: id.get_index(),
                device: id.get_device(),
                subdevice: id.get_subdevice(),
                kind,
                values,
                grouped_label: "Other".to_string(),
                favorite: false,
            };
            ctrl.grouped_label = Self::group_label(&ctrl.name);
            controls.push(ctrl);
        }
        controls.sort_by(|a, b| a.name.cmp(&b.name).then(a.numid.cmp(&b.numid)));
        self.refresh_kind_cache_by_numid(&controls);
        Ok(controls)
    }

    fn refresh_kind_cache_by_numid(&self, controls: &[ControlDescriptor]) {
        if let Ok(mut cache) = self.kind_cache_by_numid.lock() {
            cache.clear();
            for c in controls {
                cache.insert(c.numid, c.kind.clone());
            }
        }
    }

    fn open_hctl_handle(card_index: u32) -> Result<HCtl> {
        let hctl = HCtl::new(&format!("hw:{card_index}"), false)
            .context("Failed to open ALSA hctl device")?;
        hctl.load().context("Failed to load ALSA hctl elements")?;
        Ok(hctl)
    }

    fn open_ctl_handle(card_index: u32) -> Result<Ctl> {
        Ctl::new(&format!("hw:{card_index}"), false).context("Failed to open ALSA ctl device")
    }

    fn channels_from_kind(kind: &ControlKind) -> usize {
        match kind {
            ControlKind::Integer { channels, .. }
            | ControlKind::Boolean { channels }
            | ControlKind::Enumerated { channels, .. }
            | ControlKind::Unknown { channels, .. } => *channels,
        }
    }

    fn infer_control_kind_from_elem(
        id: &alsa::ctl::ElemId,
        info: &alsa::ctl::ElemInfo,
        ctl: &Ctl,
    ) -> Result<ControlKind> {
        let count = info.get_count() as usize;
        let info_ptr = Self::elem_info_ptr(info);
        let kind = match info.get_type() {
            ElemType::Integer => {
                let min = unsafe { alsa_ffi::snd_ctl_elem_info_get_min(info_ptr) as i64 };
                let mut max = unsafe { alsa_ffi::snd_ctl_elem_info_get_max(info_ptr) as i64 };
                let step = unsafe { alsa_ffi::snd_ctl_elem_info_get_step(info_ptr) as i64 }.max(1);
                if max <= min {
                    max = min + 1;
                }
                let db_range = Self::lookup_db_range_for_control(ctl, id, min, max);
                ControlKind::Integer {
                    min,
                    max,
                    step,
                    channels: count.max(1),
                    db_range,
                }
            }
            ElemType::Integer64 => {
                let min = unsafe { alsa_ffi::snd_ctl_elem_info_get_min64(info_ptr) as i64 };
                let mut max = unsafe { alsa_ffi::snd_ctl_elem_info_get_max64(info_ptr) as i64 };
                let step = unsafe { alsa_ffi::snd_ctl_elem_info_get_step64(info_ptr) as i64 }.max(1);
                if max <= min {
                    max = min + 1;
                }
                ControlKind::Integer {
                    min,
                    max,
                    step,
                    channels: count.max(1),
                    db_range: None,
                }
            }
            ElemType::Boolean => ControlKind::Boolean {
                channels: count.max(1),
            },
            ElemType::Enumerated => {
                let item_count = unsafe { alsa_ffi::snd_ctl_elem_info_get_items(info_ptr) as usize }.max(1);
                let items = (0..item_count).map(|i| i.to_string()).collect();
                ControlKind::Enumerated {
                    items,
                    channels: count.max(1),
                }
            }
            other => ControlKind::Unknown {
                type_name: format!("{other:?}"),
                channels: count.max(1),
            },
        };
        Ok(kind)
    }

    fn lookup_db_range_for_control(
        ctl: &Ctl,
        id: &alsa::ctl::ElemId,
        min: i64,
        max: i64,
    ) -> Option<(i64, i64)> {
        if max < min {
            return None;
        }
        if let Ok((db_min, db_max)) = ctl.get_db_range(id) {
            if db_max.0 > db_min.0 {
                return Some((db_min.0, db_max.0));
            }
        }
        let db_min = ctl.convert_to_db(id, min).ok()?.0;
        let db_max = ctl.convert_to_db(id, max).ok()?.0;
        if db_max > db_min {
            Some((db_min, db_max))
        } else {
            None
        }
    }

    fn elem_info_ptr(info: &alsa::ctl::ElemInfo) -> *mut alsa_ffi::snd_ctl_elem_info_t {
        unsafe { *(info as *const _ as *const *mut alsa_ffi::snd_ctl_elem_info_t) }
    }

    pub fn apply_values(&self, numid: u32, values: &[String]) -> Result<()> {
        self.apply_values_native(numid, values)
    }

    pub fn reload_control(&self, original: &ControlDescriptor) -> Result<ControlDescriptor> {
        let values = self.read_values_by_numid_from_hctl(original.numid, &original.kind)?;
        let mut out = original.clone();
        out.values = values;
        Ok(out)
    }

    pub fn refresh_control_values(&self, controls: &mut [ControlDescriptor]) -> Result<usize> {
        self.refresh_control_values_native(controls)
    }

    fn refresh_control_values_native(&self, controls: &mut [ControlDescriptor]) -> Result<usize> {
        let hctl = self
            .hctl_handle
            .as_ref()
            .ok_or_else(|| anyhow!("Native ALSA backend not initialized"))?;
        let index_by_numid: HashMap<u32, usize> =
            controls.iter().enumerate().map(|(i, c)| (c.numid, i)).collect();
        let mut updated = 0usize;

        for elem in hctl.elem_iter() {
            let id = elem.get_id()?;
            let Some(ctrl_idx) = index_by_numid.get(&id.get_numid()).copied() else {
                continue;
            };
            let kind = controls[ctrl_idx].kind.clone();
            let new_values = self.read_values_from_elem_for_kind(&elem, &kind)?;
            if controls[ctrl_idx].values != new_values {
                controls[ctrl_idx].values = new_values;
                updated += 1;
            }
        }
        Ok(updated)
    }

    fn read_values_by_numid_from_hctl(&self, numid: u32, kind: &ControlKind) -> Result<Vec<String>> {
        let hctl = self
            .hctl_handle
            .as_ref()
            .ok_or_else(|| anyhow!("Native ALSA backend not initialized"))?;
        for elem in hctl.elem_iter() {
            let id = elem.get_id()?;
            if id.get_numid() == numid {
                return self.read_values_from_elem_for_kind(&elem, kind);
            }
        }
        bail!("Control numid={numid} not found in native backend");
    }

    fn read_values_from_elem_for_kind(
        &self,
        elem: &alsa::hctl::Elem<'_>,
        kind: &ControlKind,
    ) -> Result<Vec<String>> {
        let value = elem.read()?;
        let out = match kind {
            ControlKind::Integer { channels, .. } => {
                let mut vals = Vec::new();
                for ch in 0..*channels {
                    if let Some(v) = value.get_integer(ch as u32) {
                        vals.push(v.to_string());
                    } else if let Some(v) = value.get_integer64(ch as u32) {
                        vals.push(v.to_string());
                    }
                }
                vals
            }
            ControlKind::Boolean { channels } => {
                let mut vals = Vec::new();
                for ch in 0..*channels {
                    if let Some(v) = value.get_boolean(ch as u32) {
                        vals.push(if v { "on" } else { "off" }.to_string());
                    }
                }
                vals
            }
            ControlKind::Enumerated { items, channels } => {
                let mut vals = Vec::new();
                for ch in 0..*channels {
                    if let Some(idx) = value.get_enumerated(ch as u32) {
                        vals.push(
                            items
                                .get(idx as usize)
                                .cloned()
                                .unwrap_or_else(|| idx.to_string()),
                        );
                    }
                }
                vals
            }
            ControlKind::Unknown { channels, .. } => {
                let mut vals = Vec::new();
                for ch in 0..*channels {
                    if let Some(v) = value.get_integer(ch as u32) {
                        vals.push(v.to_string());
                    } else if let Some(v) = value.get_boolean(ch as u32) {
                        vals.push(if v { "on" } else { "off" }.to_string());
                    } else if let Some(v) = value.get_enumerated(ch as u32) {
                        vals.push(v.to_string());
                    }
                }
                vals
            }
        };
        Ok(out)
    }

    fn apply_values_native(&self, numid: u32, values: &[String]) -> Result<()> {
        let hctl = self
            .hctl_handle
            .as_ref()
            .ok_or_else(|| anyhow!("Native ALSA backend not initialized"))?;
        let control_kind = self
            .kind_cache_by_numid
            .lock()
            .ok()
            .and_then(|cache| cache.get(&numid).cloned());

        for elem in hctl.elem_iter() {
            let id = elem.get_id()?;
            if id.get_numid() != numid {
                continue;
            }
            let info = elem.info()?;
            let mut current = elem.read()?;
            let count = info.get_count() as usize;
            Self::set_elem_values_from_input(
                &mut current,
                info.get_type(),
                count,
                values,
                control_kind.as_ref(),
            );
            let _ = elem.write(&current)?;
            if !Self::first_channel_matches_target(
                &elem,
                info.get_type(),
                values,
                control_kind.as_ref(),
            ) {
                thread::sleep(Duration::from_millis(8));
                let mut retry = elem.read()?;
                Self::set_elem_values_from_input(
                    &mut retry,
                    info.get_type(),
                    count,
                    values,
                    control_kind.as_ref(),
                );
                let _ = elem.write(&retry)?;
            }
            return Ok(());
        }
        bail!("Control numid={numid} not found in native backend");
    }

    fn value_at_or_first_or_default<'a>(values: &'a [String], ch: usize, default: &'a str) -> &'a str {
        values
            .get(ch)
            .or_else(|| values.first())
            .map(String::as_str)
            .unwrap_or(default)
    }

    fn parse_enum_value_index(raw: &str, control_kind: Option<&ControlKind>) -> u32 {
        if let Some(ControlKind::Enumerated { items, .. }) = control_kind {
            items
                .iter()
                .position(|item| item.eq_ignore_ascii_case(raw))
                .unwrap_or_else(|| raw.parse::<usize>().unwrap_or(0)) as u32
        } else {
            raw.parse::<u32>().unwrap_or(0)
        }
    }

    fn set_elem_values_from_input(
        value: &mut alsa::ctl::ElemValue,
        elem_type: ElemType,
        count: usize,
        values: &[String],
        control_kind: Option<&ControlKind>,
    ) {
        match elem_type {
            ElemType::Integer => {
                for ch in 0..count {
                    let mut parsed = Self::value_at_or_first_or_default(values, ch, "0")
                        .parse::<i64>()
                        .unwrap_or(0);
                    if let Some(ControlKind::Integer { min, max, .. }) = control_kind {
                        parsed = parsed.clamp(*min, *max);
                    }
                    let parsed = if parsed < i32::MIN as i64 {
                        i32::MIN
                    } else if parsed > i32::MAX as i64 {
                        i32::MAX
                    } else {
                        parsed as i32
                    };
                    let _ = value.set_integer(ch as u32, parsed);
                }
            }
            ElemType::Integer64 => {
                for ch in 0..count {
                    let mut parsed = Self::value_at_or_first_or_default(values, ch, "0")
                        .parse::<i64>()
                        .unwrap_or(0);
                    if let Some(ControlKind::Integer { min, max, .. }) = control_kind {
                        parsed = parsed.clamp(*min, *max);
                    }
                    let _ = value.set_integer64(ch as u32, parsed);
                }
            }
            ElemType::Boolean => {
                for ch in 0..count {
                    let raw = Self::value_at_or_first_or_default(values, ch, "off");
                    let on =
                        raw.eq_ignore_ascii_case("on") || raw.eq_ignore_ascii_case("true") || raw == "1";
                    let _ = value.set_boolean(ch as u32, on);
                }
            }
            ElemType::Enumerated => {
                for ch in 0..count {
                    let raw = Self::value_at_or_first_or_default(values, ch, "0");
                    let idx = Self::parse_enum_value_index(raw, control_kind);
                    let _ = value.set_enumerated(ch as u32, idx);
                }
            }
            _ => {}
        }
    }

    fn first_channel_matches_target(
        elem: &alsa::hctl::Elem<'_>,
        elem_type: ElemType,
        values: &[String],
        control_kind: Option<&ControlKind>,
    ) -> bool {
        let Ok(after) = elem.read() else {
            return false;
        };
        match elem_type {
            ElemType::Integer => after.get_integer(0).unwrap_or_default()
                == {
                    let mut expected = Self::value_at_or_first_or_default(values, 0, "0")
                        .parse::<i64>()
                        .unwrap_or(0);
                    if let Some(ControlKind::Integer { min, max, .. }) = control_kind {
                        expected = expected.clamp(*min, *max);
                    }
                    expected
                        .try_into()
                        .unwrap_or(if expected < i32::MIN as i64 { i32::MIN } else { i32::MAX })
                },
            ElemType::Integer64 => after.get_integer64(0).unwrap_or_default()
                == {
                    let mut expected = Self::value_at_or_first_or_default(values, 0, "0")
                        .parse::<i64>()
                        .unwrap_or(0);
                    if let Some(ControlKind::Integer { min, max, .. }) = control_kind {
                        expected = expected.clamp(*min, *max);
                    }
                    expected
                },
            ElemType::Boolean => {
                let raw = Self::value_at_or_first_or_default(values, 0, "off");
                let on = raw.eq_ignore_ascii_case("on")
                    || raw.eq_ignore_ascii_case("true")
                    || raw == "1";
                after.get_boolean(0).unwrap_or(false) == on
            }
            ElemType::Enumerated => {
                let expected = Self::parse_enum_value_index(
                    Self::value_at_or_first_or_default(values, 0, "0"),
                    control_kind,
                );
                after.get_enumerated(0).unwrap_or_default() == expected
            }
            _ => true,
        }
    }

    pub fn build_routing_index(controls: &[ControlDescriptor]) -> RoutingIndex {
        let analog_re = Regex::new(r"^AIn(\d+)\s*-\s*Out(\d+)(?:\b.*)?$").expect("valid regex");
        let digital_re = Regex::new(r"^DIn(\d+)\s*-\s*Out(\d+)(?:\b.*)?$").expect("valid regex");

        let mut index = RoutingIndex::default();
        for (i, c) in controls.iter().enumerate() {
            if let Some(cap) = analog_re.captures(&c.name) {
                let input = cap
                    .get(1)
                    .and_then(|m| m.as_str().parse::<usize>().ok())
                    .unwrap_or(1)
                    .saturating_sub(1);
                let output = cap
                    .get(2)
                    .and_then(|m| m.as_str().parse::<usize>().ok())
                    .unwrap_or(1)
                    .saturating_sub(1);
                index.analog_routes.push(RouteRef {
                    output,
                    input,
                    control_index: i,
                });
            } else if let Some(cap) = digital_re.captures(&c.name) {
                let input = cap
                    .get(1)
                    .and_then(|m| m.as_str().parse::<usize>().ok())
                    .unwrap_or(1)
                    .saturating_sub(1);
                let output = cap
                    .get(2)
                    .and_then(|m| m.as_str().parse::<usize>().ok())
                    .unwrap_or(1)
                    .saturating_sub(1);
                index.digital_routes.push(RouteRef {
                    output,
                    input,
                    control_index: i,
                });
            }
        }
        index
    }

    fn group_label(name: &str) -> String {
        if name.starts_with("AIn") {
            "Analog Routing".to_string()
        } else if name.starts_with("DIn") {
            "Digital Routing".to_string()
        } else if name.to_lowercase().contains("fx") || name.to_lowercase().contains("effect") {
            "Effects".to_string()
        } else {
            "Other".to_string()
        }
    }

}
