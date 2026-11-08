// Triggers Tab — trigger settings, key modes, edit modal, keyboard layout view
//
// All triggers-specific types, rendering, and App methods.

use ratatui::{prelude::*, widgets::*};
use std::collections::VecDeque;

use crate::TriggerSettings;
use crate::key_action::KeyAction;
use crate::protocol::hid;
use crate::tui::widgets::PopupSelect;
use monsgeek_keyboard::{
    DksAction, DksBinding, DksConfig, DksPhase, KeyMode, KeyTriggerSettings, ModeByte, Precision,
    TravelDepth,
};
use monsgeek_transport::protocol::{HidUsage, KeymatrixLayer, Layer};

use super::super::App;
use super::super::keys::{CONSUMER_KEYS, all_hid_keys};
use super::super::shared::{AsyncResult, LoadState, SpinnerConfig};
use super::depth::get_key_label;

// ============================================================================
// Types
// ============================================================================

/// Trigger edit modal target - what we're editing
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::tui) enum TriggerEditTarget {
    /// Edit global settings (applies to all keys)
    Global,
    /// Edit specific key settings
    PerKey { key_index: usize },
}

/// Editable field in trigger settings
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::tui) enum TriggerField {
    Actuation,
    Release,
    RtPress,
    RtLift,
    TopDeadzone,
    BottomDeadzone,
    Mode,
    RapidTrigger,
    OutputLayer,
    Output,
    ModTapTime,
    SnapTapPartner,
    DksTravel,
    DksBinding,
    DksBindingKey,
    DksAct0,
    DksAct1,
    DksAct2,
    DksAct3,
    /// Not a setting — the modal's Save button, focusable like any other row.
    Save,
}

impl TriggerField {
    const CORE_TRAVEL: &'static [TriggerField] = &[Self::Actuation, Self::Release];
    const RT_SENSITIVITY: &'static [TriggerField] = &[Self::RtPress, Self::RtLift];
    const DEADZONES: &'static [TriggerField] = &[Self::TopDeadzone, Self::BottomDeadzone];
    const MODE_CONTROLS: &'static [TriggerField] = &[Self::Mode, Self::RapidTrigger];
    /// The key's emitted output (keymatrix layer 0). Shown for every mode except
    /// DKS, where the four combo slots below are the output.
    const OUTPUT: &'static [TriggerField] = &[Self::OutputLayer, Self::Output];
    const MODTAP: &'static [TriggerField] = &[Self::ModTapTime];
    const SNAPTAP: &'static [TriggerField] = &[Self::SnapTapPartner];
    const DKS: &'static [TriggerField] = &[
        Self::DksBinding,
        Self::DksBindingKey,
        Self::DksAct0,
        Self::DksAct1,
        Self::DksAct2,
        Self::DksAct3,
    ];

    fn append_fields(out: &mut Vec<TriggerField>, slice: &'static [TriggerField]) {
        out.extend_from_slice(slice);
    }

    /// Bulk all-keys edit — only fields that `save_trigger_edit_modal` writes globally.
    fn global_fields() -> Vec<TriggerField> {
        let mut fields = Vec::new();
        Self::append_fields(&mut fields, Self::CORE_TRAVEL);
        Self::append_fields(&mut fields, Self::RT_SENSITIVITY);
        Self::append_fields(&mut fields, Self::DEADZONES);
        fields.push(Self::Save);
        fields
    }

    /// Per-key fields visible for the current base mode (and RT flag).
    ///
    /// Mode / Rapid Trigger are always first so changing mode refreshes the list below.
    fn per_key_fields(mode_byte: u8) -> Vec<TriggerField> {
        let base = KeyMode::from_u8(mode_byte);
        let rt_on = mode_byte & ModeByte::RT_FLAG != 0;
        let mut fields = Vec::new();
        Self::append_fields(&mut fields, Self::MODE_CONTROLS);
        // The output binding (keymatrix layer 0). In DKS mode the combo slots below
        // are the output, so it's omitted there.
        if base != KeyMode::DynamicKeystroke {
            Self::append_fields(&mut fields, Self::OUTPUT);
        }

        match base {
            KeyMode::Normal => {
                Self::append_fields(&mut fields, Self::CORE_TRAVEL);
                if rt_on {
                    Self::append_fields(&mut fields, Self::RT_SENSITIVITY);
                }
                Self::append_fields(&mut fields, Self::DEADZONES);
            }
            KeyMode::DynamicKeystroke => {
                Self::append_fields(&mut fields, &[Self::DksTravel, Self::Actuation]);
                if rt_on {
                    Self::append_fields(&mut fields, Self::RT_SENSITIVITY);
                }
                Self::append_fields(&mut fields, Self::DKS);
            }
            KeyMode::ModTap => {
                Self::append_fields(&mut fields, Self::MODTAP);
                if rt_on {
                    Self::append_fields(&mut fields, Self::RT_SENSITIVITY);
                }
            }
            KeyMode::SnapTap => {
                Self::append_fields(&mut fields, Self::SNAPTAP);
                if rt_on {
                    Self::append_fields(&mut fields, Self::RT_SENSITIVITY);
                }
            }
            // Toggle output is configured in the keymap; vendor hides travel/DZ for TGL.
            KeyMode::ToggleHold | KeyMode::ToggleDots => {
                if rt_on {
                    Self::append_fields(&mut fields, Self::RT_SENSITIVITY);
                }
            }
            KeyMode::Unknown(_) => {
                if rt_on {
                    Self::append_fields(&mut fields, Self::RT_SENSITIVITY);
                }
            }
        }
        fields.push(Self::Save);
        fields
    }

    /// Column label, with mode-specific names where the same field means something else.
    pub(in crate::tui) fn label_for(self, mode_byte: u8) -> &'static str {
        match (self, KeyMode::from_u8(mode_byte)) {
            (Self::Actuation, KeyMode::DynamicKeystroke) => "DKS Full Depth",
            _ => self.label(),
        }
    }

    pub(in crate::tui) fn label(&self) -> &'static str {
        match self {
            Self::Actuation => "Actuation",
            Self::Release => "Release",
            Self::RtPress => "RT Press",
            Self::RtLift => "RT Lift",
            Self::TopDeadzone => "Top DZ",
            Self::BottomDeadzone => "Bottom DZ",
            Self::Mode => "Mode",
            Self::RapidTrigger => "Rapid Trig",
            Self::OutputLayer => "Layer",
            Self::Output => "Output",
            Self::ModTapTime => "MT Time",
            Self::SnapTapPartner => "SnapTap Key",
            Self::DksTravel => "DKS Trigger Pt",
            Self::DksBinding => "DKS Binding",
            Self::DksBindingKey => "DKS Output",
            Self::DksAct0 => DksPhase::PressShallow.short_label(),
            Self::DksAct1 => DksPhase::PressFull.short_label(),
            Self::DksAct2 => DksPhase::ReleaseFull.short_label(),
            Self::DksAct3 => DksPhase::ReleaseShallow.short_label(),
            Self::Save => "Save",
        }
    }

    /// Spinner configuration for this field, or `None` for the fields that are
    /// cycled or edited through a popup.
    ///
    /// Travel bounds are stated in millimetres and converted to raw units here,
    /// once — the spinner itself then steps in raw units, so an edit cannot drift.
    pub(in crate::tui) fn spinner_config(&self, precision: Precision) -> Option<SpinnerConfig> {
        match self {
            Self::Actuation | Self::Release => {
                Some(SpinnerConfig::travel_mm(0.1, 4.0, 0.05, 0.2, precision))
            }
            Self::RtPress | Self::RtLift => {
                Some(SpinnerConfig::travel_mm(0.1, 2.0, 0.05, 0.1, precision))
            }
            Self::TopDeadzone | Self::BottomDeadzone => {
                Some(SpinnerConfig::travel_mm(0.0, 1.0, 0.05, 0.1, precision))
            }
            Self::ModTapTime => Some(SpinnerConfig::integer(0, 2550, 10, 100, "ms")),
            Self::DksTravel => Some(SpinnerConfig::travel_mm(0.1, 4.0, 0.05, 0.2, precision)),
            // Mode / SnapTapPartner / DKS pickers open popups; RapidTrigger toggles.
            Self::Mode
            | Self::RapidTrigger
            | Self::OutputLayer
            | Self::Output
            | Self::SnapTapPartner
            | Self::DksBinding
            | Self::DksBindingKey
            | Self::DksAct0
            | Self::DksAct1
            | Self::DksAct2
            | Self::DksAct3
            | Self::Save => None,
        }
    }

    fn dks_action_index(self) -> Option<usize> {
        match self {
            Self::DksAct0 => Some(0),
            Self::DksAct1 => Some(1),
            Self::DksAct2 => Some(2),
            Self::DksAct3 => Some(3),
            _ => None,
        }
    }
}

/// Fallback DKS trigger point when the device read fails.
const DEFAULT_DKS_TRAVEL_MM: f64 = 0.7;

/// DKS fields loaded when opening a per-key trigger edit modal.
#[derive(Debug, Clone)]
pub(in crate::tui) struct DksEditState {
    pub travel_raw: u16,
    /// Phase roles per output slot; the slot outputs themselves live in
    /// [`PerKeyEditPrefetch::slots`], since they *are* the keymatrix layers.
    pub phases: [[DksAction; 4]; 4],
}

/// Per-key sub-configs fetched from the device before opening the edit modal.
#[derive(Debug, Clone)]
pub(in crate::tui) struct PerKeyEditPrefetch {
    pub modtap_ms: u16,
    pub snaptap_partner: Option<u8>,
    pub key_choices: Vec<(String, u8)>,
    pub dks: DksEditState,
    /// Keymatrix layers 0–3. In normal modes layers 0/1 are the key's Base and
    /// Layer1 outputs; in DKS mode the firmware reinterprets all four as the key's
    /// output slots. One array either way — the mode only changes presentation.
    pub slots: [KeyAction; 4],
    /// The key's Fn-layer entry, which lives in a separate store (SET_FN).
    pub fn_action: KeyAction,
}

/// Trigger edit modal state
#[derive(Debug, Clone)]
pub(in crate::tui) struct TriggerEditModal {
    /// What we're editing (global or per-key)
    pub target: TriggerEditTarget,
    /// Currently focused field
    pub field_index: usize,
    /// Depth history for the chart (samples over time)
    pub depth_history: VecDeque<f32>,
    /// Key to filter depth reports (None = show all active keys)
    pub depth_filter: Option<usize>,
    /// Device travel precision, for rendering and for sizing the spinners.
    pub precision: Precision,
    /// Current values being edited, in raw firmware units — the form they are
    /// stored and sent in, so opening and closing unchanged is bit-identical.
    pub actuation: TravelDepth,
    pub release: TravelDepth,
    pub rt_press: TravelDepth,
    pub rt_lift: TravelDepth,
    pub top_dz: TravelDepth,
    pub bottom_dz: TravelDepth,
    /// Full mode byte (base mode in low 7 bits, RT flag in `0x80`)
    pub mode: u8,
    /// Mod-Tap tap-vs-hold decision time in ms (per-key only)
    pub modtap_ms: u16,
    /// Snap-Tap partner key index, if bound (per-key only)
    pub snaptap_partner: Option<u8>,
    /// `(label, key_index)` choices for the Snap-Tap partner picker
    pub key_choices: Vec<(String, u8)>,
    /// Open base-mode picker, when the user is choosing a mode
    pub mode_picker: Option<PopupSelect<KeyMode>>,
    /// Open Snap-Tap partner picker, when choosing a partner key (`None` = unbound)
    pub key_picker: Option<PopupSelect<Option<u8>>>,
    /// DKS trigger-point travel (per-key only; R1/R4 shallow depth)
    pub dks_travel: TravelDepth,
    /// Phase roles per DKS output slot (per-key only)
    pub dks_phases: [[DksAction; 4]; 4],
    /// Which DKS slot (0–3) the DKS fields edit
    pub dks_binding_index: usize,
    /// Open DKS action picker: `(DksPhase index, picker)`
    pub dks_action_picker: Option<(usize, PopupSelect<DksAction>)>,
    /// Keymatrix layers 0–3 (see [`PerKeyEditPrefetch::slots`]).
    pub slots: [KeyAction; 4],
    /// The key's Fn-layer entry.
    pub fn_action: KeyAction,
    /// Snapshots at open, so save only writes what changed.
    pub slots_orig: [KeyAction; 4],
    pub fn_action_orig: KeyAction,
    /// Which output the `Output`/`Layer` fields target: 0=Base, 1=Layer1, 2=Fn.
    pub output_layer: Layer,
    /// Open output picker: any HID key or consumer/media control. Shared by the
    /// per-layer output field and the DKS slot field — both edit a keymatrix entry.
    pub output_picker: Option<PopupSelect<KeyAction>>,
    /// Usages staged with Tab while the output picker is open, so a slot can be set
    /// to a chord (`Ctrl+C`) rather than a single key. Empty = picking one key.
    pub chord_buf: Vec<HidUsage>,
    /// The output picker's title without the chord preview, for rebuilding it.
    pub picker_title: String,
}

impl TriggerEditModal {
    /// Create modal for editing global settings
    pub(in crate::tui) fn new_global(triggers: &TriggerSettings, precision: Precision) -> Self {
        let first = |v: &Vec<u16>| TravelDepth::from_raw(v.first().copied().unwrap_or(0));
        Self {
            target: TriggerEditTarget::Global,
            field_index: 0,
            depth_history: VecDeque::with_capacity(100),
            depth_filter: None,
            precision,
            actuation: first(&triggers.press_travel),
            release: first(&triggers.lift_travel),
            rt_press: first(&triggers.rt_press),
            rt_lift: first(&triggers.rt_lift),
            top_dz: first(&triggers.top_deadzone),
            bottom_dz: first(&triggers.bottom_deadzone),
            mode: triggers.key_modes.first().copied().unwrap_or(0),
            modtap_ms: 0,
            snaptap_partner: None,
            key_choices: Vec::new(),
            mode_picker: None,
            key_picker: None,
            dks_travel: TravelDepth::default(),
            dks_phases: [[DksAction::default(); 4]; 4],
            dks_binding_index: 0,
            dks_action_picker: None,
            slots: [KeyAction::Disabled; 4],
            fn_action: KeyAction::Disabled,
            slots_orig: [KeyAction::Disabled; 4],
            fn_action_orig: KeyAction::Disabled,
            output_layer: Layer::Base,
            output_picker: None,
            chord_buf: Vec::new(),
            picker_title: String::new(),
        }
    }

    /// Create modal for editing a specific key. `prefetch` is loaded by the
    /// caller (needs device access for Mod-Tap, Snap-Tap, and DKS).
    pub(in crate::tui) fn new_per_key(
        key_index: usize,
        triggers: &TriggerSettings,
        precision: Precision,
        prefetch: PerKeyEditPrefetch,
    ) -> Self {
        let at = |v: &Vec<u16>| TravelDepth::from_raw(v.get(key_index).copied().unwrap_or(0));
        Self {
            target: TriggerEditTarget::PerKey { key_index },
            field_index: 0,
            depth_history: VecDeque::with_capacity(100),
            depth_filter: Some(key_index),
            precision,
            actuation: at(&triggers.press_travel),
            release: at(&triggers.lift_travel),
            rt_press: at(&triggers.rt_press),
            rt_lift: at(&triggers.rt_lift),
            top_dz: at(&triggers.top_deadzone),
            bottom_dz: at(&triggers.bottom_deadzone),
            mode: triggers.key_modes.get(key_index).copied().unwrap_or(0),
            modtap_ms: prefetch.modtap_ms,
            snaptap_partner: prefetch.snaptap_partner,
            key_choices: prefetch.key_choices,
            mode_picker: None,
            key_picker: None,
            dks_travel: TravelDepth::from_raw(prefetch.dks.travel_raw),
            dks_phases: prefetch.dks.phases,
            dks_binding_index: 0,
            dks_action_picker: None,
            slots: prefetch.slots,
            fn_action: prefetch.fn_action,
            slots_orig: prefetch.slots,
            fn_action_orig: prefetch.fn_action,
            output_layer: Layer::Base,
            output_picker: None,
            chord_buf: Vec::new(),
            picker_title: String::new(),
        }
    }

    /// Fields shown for this modal, filtered to what applies to the current mode.
    pub(in crate::tui) fn visible_fields(&self) -> Vec<TriggerField> {
        match self.target {
            TriggerEditTarget::Global => TriggerField::global_fields(),
            TriggerEditTarget::PerKey { .. } => TriggerField::per_key_fields(self.mode),
        }
    }

    /// Keep focus on `preferred` after the visible field list changes (e.g. mode switch).
    fn clamp_field_index(&mut self, preferred: TriggerField) {
        let fields = self.visible_fields();
        self.field_index = fields
            .iter()
            .position(|&f| f == preferred)
            .or_else(|| fields.iter().position(|&f| f == TriggerField::Mode))
            .unwrap_or(0);
    }

    pub(in crate::tui) fn current_field(&self) -> TriggerField {
        let fields = self.visible_fields();
        fields
            .get(self.field_index)
            .copied()
            .unwrap_or(TriggerField::Mode)
    }

    pub(in crate::tui) fn next_field(&mut self) {
        let len = self.visible_fields().len().max(1);
        self.field_index = (self.field_index + 1) % len;
    }

    pub(in crate::tui) fn prev_field(&mut self) {
        let len = self.visible_fields().len().max(1);
        self.field_index = if self.field_index == 0 {
            len - 1
        } else {
            self.field_index - 1
        };
    }

    /// Get the current value for the selected spinner field (0.0 for
    /// non-spinner fields, which are edited by other means).
    pub(in crate::tui) fn current_value(&self) -> u16 {
        self.value_of(self.current_field())
    }

    /// The raw value backing a spinner field (0 for the fields edited by other
    /// means). Shared by the key handler and the renderer.
    pub(in crate::tui) fn value_of(&self, field: TriggerField) -> u16 {
        match field {
            TriggerField::Actuation => self.actuation.raw(),
            TriggerField::Release => self.release.raw(),
            TriggerField::RtPress => self.rt_press.raw(),
            TriggerField::RtLift => self.rt_lift.raw(),
            TriggerField::TopDeadzone => self.top_dz.raw(),
            TriggerField::BottomDeadzone => self.bottom_dz.raw(),
            TriggerField::ModTapTime => self.modtap_ms,
            TriggerField::DksTravel => self.dks_travel.raw(),
            TriggerField::Mode
            | TriggerField::RapidTrigger
            | TriggerField::OutputLayer
            | TriggerField::Output
            | TriggerField::SnapTapPartner
            | TriggerField::DksBinding
            | TriggerField::DksBindingKey
            | TriggerField::DksAct0
            | TriggerField::DksAct1
            | TriggerField::DksAct2
            | TriggerField::DksAct3
            | TriggerField::Save => 0,
        }
    }

    /// Set the value for the selected spinner field.
    pub(in crate::tui) fn set_current_value(&mut self, value: u16) {
        match self.current_field() {
            TriggerField::Actuation => self.actuation = TravelDepth::from_raw(value),
            TriggerField::Release => self.release = TravelDepth::from_raw(value),
            TriggerField::RtPress => self.rt_press = TravelDepth::from_raw(value),
            TriggerField::RtLift => self.rt_lift = TravelDepth::from_raw(value),
            TriggerField::TopDeadzone => self.top_dz = TravelDepth::from_raw(value),
            TriggerField::BottomDeadzone => self.bottom_dz = TravelDepth::from_raw(value),
            TriggerField::ModTapTime => self.modtap_ms = value,
            TriggerField::DksTravel => self.dks_travel = TravelDepth::from_raw(value),
            TriggerField::Mode
            | TriggerField::RapidTrigger
            | TriggerField::OutputLayer
            | TriggerField::Output
            | TriggerField::SnapTapPartner
            | TriggerField::DksBinding
            | TriggerField::DksBindingKey
            | TriggerField::DksAct0
            | TriggerField::DksAct1
            | TriggerField::DksAct2
            | TriggerField::DksAct3
            | TriggerField::Save => {}
        }
    }

    /// Act on the focused field, as Enter does: open its picker, flip a toggle,
    /// cycle a selector. Returns true when the field is the Save button, which the
    /// caller commits — spinner rows do nothing, since ←/→ is how they're adjusted.
    #[must_use]
    pub(in crate::tui) fn activate_current(&mut self) -> bool {
        match self.current_field() {
            TriggerField::Save => return true,
            TriggerField::Mode => self.open_mode_picker(),
            TriggerField::OutputLayer => self.cycle_output_layer(true),
            TriggerField::Output => self.open_output_picker(),
            TriggerField::SnapTapPartner => self.open_key_picker(),
            TriggerField::DksBindingKey => self.open_dks_slot_picker(),
            TriggerField::DksBinding => self.cycle_dks_binding(true),
            TriggerField::RapidTrigger => self.toggle_rapid_trigger(),
            field if field.dks_action_index().is_some() => {
                self.open_dks_action_picker(field.dks_action_index().unwrap());
            }
            _ => {}
        }
        false
    }

    /// Increment the current field: spinner up, or open a picker / flip the RT
    /// flag for the non-spinner fields.
    pub(in crate::tui) fn increment_current(&mut self, coarse: bool) {
        match self.current_field() {
            TriggerField::Mode => self.open_mode_picker(),
            TriggerField::OutputLayer => self.cycle_output_layer(true),
            TriggerField::Output => self.open_output_picker(),
            TriggerField::SnapTapPartner => self.open_key_picker(),
            TriggerField::DksBindingKey => self.open_dks_slot_picker(),
            TriggerField::DksBinding => self.cycle_dks_binding(true),
            field if field.dks_action_index().is_some() => {
                self.open_dks_action_picker(field.dks_action_index().unwrap());
            }
            TriggerField::RapidTrigger => self.toggle_rapid_trigger(),
            field => {
                if let Some(config) = field.spinner_config(self.precision) {
                    let new_value = config.increment(self.current_value(), coarse);
                    self.set_current_value(new_value);
                }
            }
        }
    }

    /// Decrement the current field: spinner down, or open a picker / flip the RT
    /// flag for the non-spinner fields.
    pub(in crate::tui) fn decrement_current(&mut self, coarse: bool) {
        match self.current_field() {
            TriggerField::Mode => self.open_mode_picker(),
            TriggerField::OutputLayer => self.cycle_output_layer(false),
            TriggerField::Output => self.open_output_picker(),
            TriggerField::SnapTapPartner => self.open_key_picker(),
            TriggerField::DksBindingKey => self.open_dks_slot_picker(),
            TriggerField::DksBinding => self.cycle_dks_binding(false),
            field if field.dks_action_index().is_some() => {
                self.open_dks_action_picker(field.dks_action_index().unwrap());
            }
            TriggerField::RapidTrigger => self.toggle_rapid_trigger(),
            field => {
                if let Some(config) = field.spinner_config(self.precision) {
                    let new_value = config.decrement(self.current_value(), coarse);
                    self.set_current_value(new_value);
                }
            }
        }
    }

    fn cycle_dks_binding(&mut self, forward: bool) {
        if forward {
            self.dks_binding_index = (self.dks_binding_index + 1) % 4;
        } else {
            self.dks_binding_index = (self.dks_binding_index + 3) % 4;
        }
    }

    /// Flip the Rapid-Trigger (`0x80`) flag, preserving the base mode.
    pub(in crate::tui) fn toggle_rapid_trigger(&mut self) {
        let preferred = TriggerField::RapidTrigger;
        self.mode ^= ModeByte::RT_FLAG;
        self.clamp_field_index(preferred);
    }

    /// Open the base-mode popup selector, preselected to the current base mode.
    pub(in crate::tui) fn open_mode_picker(&mut self) {
        let current = KeyMode::from_u8(self.mode);
        let items: Vec<(String, KeyMode)> = KeyMode::ALL
            .iter()
            .map(|&m| (m.label().to_string(), m))
            .collect();
        let mut picker = PopupSelect::new("Mode", items);
        picker.select_where(|&m| m == current);
        self.mode_picker = Some(picker);
    }

    /// Apply the picker's selection to the base mode (keeping the RT flag) and
    /// close it.
    pub(in crate::tui) fn confirm_mode_picker(&mut self) {
        if let Some(picker) = self.mode_picker.take()
            && let Some(&base) = picker.selected()
        {
            let rt = self.mode & ModeByte::RT_FLAG != 0;
            let preferred = self.current_field();
            self.mode = ModeByte::new(base, rt).to_u8();
            self.clamp_field_index(preferred);
        }
    }

    /// Open the Snap-Tap partner picker, preselected to the current partner.
    pub(in crate::tui) fn open_key_picker(&mut self) {
        let mut items = vec![("(none)".to_string(), None)];
        items.extend(self.key_choices.iter().map(|(l, i)| (l.clone(), Some(*i))));
        let mut picker = PopupSelect::new("SnapTap partner", items);
        let current = self.snaptap_partner;
        picker.select_where(|&p| p == current);
        self.key_picker = Some(picker);
    }

    /// Apply the partner-picker selection and close it.
    pub(in crate::tui) fn confirm_key_picker(&mut self) {
        if let Some(picker) = self.key_picker.take()
            && let Some(&partner) = picker.selected()
        {
            self.snaptap_partner = partner;
        }
    }

    /// The action the `Output` field currently targets: keymatrix layer 0/1, or the
    /// separate Fn entry.
    pub(in crate::tui) fn output_action(&self) -> KeyAction {
        match self.output_layer.keymatrix_layer() {
            Some(km) => self.slots[usize::from(km.get())],
            None => self.fn_action,
        }
    }

    pub(in crate::tui) fn set_output_action(&mut self, action: KeyAction) {
        match self.output_layer.keymatrix_layer() {
            Some(km) => self.slots[usize::from(km.get())] = action,
            None => self.fn_action = action,
        }
    }

    pub(in crate::tui) fn output_layer_name(&self) -> &'static str {
        match self.output_layer {
            Layer::Base => "Base",
            Layer::Layer1 => "Layer1",
            Layer::Fn => "Fn",
        }
    }

    /// Cycle the output-layer selector (Base → Layer1 → Fn → Base).
    pub(in crate::tui) fn cycle_output_layer(&mut self, forward: bool) {
        let i = Layer::ALL
            .iter()
            .position(|&l| l == self.output_layer)
            .unwrap_or(0);
        self.output_layer = if forward {
            Layer::ALL[(i + 1) % Layer::ALL.len()]
        } else {
            Layer::ALL[(i + Layer::ALL.len() - 1) % Layer::ALL.len()]
        };
    }

    /// Open the output picker for the current output layer. Offers every HID key
    /// plus the consumer/media controls — not just keys physically present on the
    /// board — so media, PrintScreen, F13-F24 etc. are all bindable. "(none)" is
    /// offered on the overlay layers (Layer1 / Fn), which are transparent when
    /// empty, but never on Base, where an empty entry would silence the key. Type
    /// to filter the list.
    pub(in crate::tui) fn open_output_picker(&mut self) {
        let (title, current) = (self.output_layer_name().to_string(), self.output_action());
        // Base (keymatrix layer 0) has no ROM fallback, so "(none)" would silence the
        // key; the overlay layers treat an empty entry as transparent.
        self.open_action_picker(title, current, self.output_layer != Layer::Base);
    }

    /// Open the same picker for the selected DKS output slot.
    pub(in crate::tui) fn open_dks_slot_picker(&mut self) {
        let slot = self.dks_binding_index;
        let title = format!("DKS slot {}", slot + 1);
        self.open_action_picker(title, self.slots[slot], slot != 0);
    }

    fn open_action_picker(&mut self, title: String, current: KeyAction, allow_none: bool) {
        let mut items: Vec<(String, KeyAction)> = Vec::new();
        if allow_none {
            items.push(("(none)".to_string(), KeyAction::Disabled));
        }
        items.extend(
            all_hid_keys()
                .into_iter()
                .map(|(code, name)| (name.to_string(), KeyAction::Key(code))),
        );
        items.extend(
            CONSUMER_KEYS
                .iter()
                .map(|&(code, name)| (format!("{name} (media)"), KeyAction::Consumer(code))),
        );
        self.picker_title = format!("{title} output");
        self.chord_buf.clear();
        let mut picker = PopupSelect::new(self.picker_title.clone(), items)
            .with_hint("type: filter   Tab: add to chord   Enter: confirm   Esc: cancel");
        picker.select_where(|&a| a == current);
        self.output_picker = Some(picker);
    }

    /// Stage/unstage the highlighted key as part of a chord (Tab in the picker).
    ///
    /// Only plain keys can be chorded: the three usage slots all live under
    /// config_type 0, so a media key or macro occupies the whole entry.
    pub(in crate::tui) fn toggle_chord_key(&mut self) {
        let Some(&KeyAction::Key(usage)) = self.output_picker.as_ref().and_then(|p| p.selected())
        else {
            return;
        };
        if let Some(pos) = self.chord_buf.iter().position(|&u| u == usage) {
            self.chord_buf.remove(pos);
        } else if self.chord_buf.len() < crate::key_action::CHORD_SLOTS {
            self.chord_buf.push(usage);
        }
        let staged = self.chord_preview();
        if let Some(p) = self.output_picker.as_mut() {
            p.set_title(staged);
            // Drop the search text so the next key of the chord can be typed
            // straight away instead of backspacing over the previous one.
            p.clear_filter();
        }
    }

    fn chord_preview(&self) -> String {
        if self.chord_buf.is_empty() {
            return self.picker_title.clone();
        }
        let keys = self
            .chord_buf
            .iter()
            .map(|&u| hid::key_name(u))
            .collect::<Vec<_>>()
            .join("+");
        format!("{} [{}+…]", self.picker_title, keys)
    }

    /// The action the picker should commit: the staged chord plus whatever is
    /// highlighted, or just the highlighted action when nothing is staged.
    pub(in crate::tui) fn picked_action(&self, highlighted: KeyAction) -> KeyAction {
        if self.chord_buf.is_empty() {
            return highlighted;
        }
        let mut usages = self.chord_buf.clone();
        if let KeyAction::Key(u) = highlighted
            && !usages.contains(&u)
        {
            usages.push(u);
        }
        KeyAction::chord(usages)
    }

    pub(in crate::tui) fn open_dks_action_picker(&mut self, action_idx: usize) {
        let current = self.dks_phases[self.dks_binding_index][action_idx];
        let items: Vec<(String, DksAction)> = [
            DksAction::None,
            DksAction::SingleTrigger,
            DksAction::ContinuousUntilNext,
            DksAction::ContinuousAcross,
        ]
        .into_iter()
        .map(|a| (a.to_string(), a))
        .collect();
        let phase = DksPhase::from_index(action_idx).unwrap_or(DksPhase::PressShallow);
        let mut picker = PopupSelect::new(
            format!(
                "DKS binding {} {}",
                self.dks_binding_index + 1,
                phase.short_label()
            ),
            items,
        );
        picker.select_where(|&a| a == current);
        self.dks_action_picker = Some((action_idx, picker));
    }

    /// Apply the DKS action-picker selection and close it.
    pub(in crate::tui) fn confirm_dks_action_picker(&mut self) {
        if let Some((idx, picker)) = self.dks_action_picker.take()
            && let Some(&action) = picker.selected()
        {
            self.dks_phases[self.dks_binding_index][idx] = action;
        }
    }

    /// Add a depth sample to history
    pub(in crate::tui) fn push_depth(&mut self, depth_mm: f32) {
        if self.depth_history.len() >= 100 {
            self.depth_history.pop_front();
        }
        self.depth_history.push_back(depth_mm);
    }
}

// ============================================================================
// App methods
// ============================================================================

impl App {
    /// Load trigger settings (tab 2).
    /// Spawns a background task to avoid blocking the UI.
    pub(in crate::tui) fn load_triggers(&mut self) {
        let Some(keyboard) = self.keyboard.clone() else {
            return;
        };

        self.loading.triggers = LoadState::Loading;
        let tx = self.gen_sender();
        tokio::spawn(async move {
            let result = keyboard
                .get_all_triggers()
                .map(|triggers| TriggerSettings {
                    press_travel: triggers.press_travel,
                    lift_travel: triggers.lift_travel,
                    rt_press: triggers.rt_press,
                    rt_lift: triggers.rt_lift,
                    key_modes: triggers.key_modes,
                    bottom_deadzone: triggers.bottom_deadzone,
                    top_deadzone: triggers.top_deadzone,
                })
                .map_err(|e| e.to_string());
            tx.send(AsyncResult::Triggers(result));
        });
    }

    /// Open trigger edit modal for global settings
    pub(in crate::tui) fn open_trigger_edit_global(&mut self) {
        if let Some(ref triggers) = self.triggers {
            let modal = TriggerEditModal::new_global(triggers, self.precision);
            self.trigger_edit_modal = Some(modal);
            // Enable depth monitoring for the modal
            if !self.depth_monitoring {
                if let Some(ref keyboard) = self.keyboard {
                    let _ = keyboard.start_magnetism_report();
                }
                self.depth_monitoring = true;
            }
            self.status_msg = "Editing global triggers (press keys to see depth)".to_string();
        } else {
            self.status_msg = "No trigger data loaded".to_string();
        }
    }

    /// Build `(label, key_index)` choices for the Snap-Tap partner picker,
    /// covering every named key.
    fn key_choices(&self) -> Vec<(String, u8)> {
        let count = self
            .triggers
            .as_ref()
            .map(|t| t.key_modes.len())
            .unwrap_or(0)
            .min(u8::MAX as usize);
        (0..count)
            .filter_map(|i| {
                let name = get_key_label(self, i);
                (!name.is_empty()).then_some((name, i as u8))
            })
            .collect()
    }

    /// Open trigger edit modal for a specific key
    pub(in crate::tui) fn open_trigger_edit_key(&mut self, key_index: usize) {
        if self.triggers.is_none() {
            self.status_msg = "No trigger data loaded".to_string();
            return;
        }
        // Best-effort fetch of the per-key sub-configs (Mod-Tap time, Snap-Tap
        // partner) that live outside the bulk trigger snapshot.
        let (modtap_ms, snaptap_partner) = match self.keyboard.as_ref() {
            Some(kb) => {
                let mt = kb
                    .get_modtap_times()
                    .ok()
                    .and_then(|v| v.get(key_index).copied())
                    .unwrap_or(0);
                let sp = kb
                    .get_snaptap_binds()
                    .ok()
                    .and_then(|v| v.get(key_index).copied())
                    .filter(|&p| p != monsgeek_keyboard::SNAPTAP_UNBOUND);
                (mt, sp)
            }
            None => (0, None),
        };
        let key_choices = self.key_choices();
        let precision = self.precision;
        let kb = self.keyboard.as_ref();

        // Keymatrix layers 0–3 and the DKS travel/phase data come from one read:
        // the DKS "bindings" *are* those layers, so reading them separately would
        // fetch the same bytes twice and risk the two copies disagreeing.
        let dks_cfg = kb.and_then(|kb| kb.get_dks_config(key_index as u8).ok());
        let slots: [KeyAction; 4] = match &dks_cfg {
            Some(cfg) => {
                std::array::from_fn(|i| KeyAction::from_config_bytes(cfg.bindings[i].config))
            }
            None => std::array::from_fn(|i| {
                kb.and_then(|kb| {
                    kb.get_key_config_at_layer(
                        kb.active_profile(),
                        KeymatrixLayer::ALL[i],
                        key_index as u8,
                    )
                    .ok()
                })
                .map(KeyAction::from_config_bytes)
                .unwrap_or(KeyAction::Disabled)
            }),
        };
        let dks = DksEditState {
            travel_raw: dks_cfg
                .as_ref()
                .map(|c| c.trigger_point_travel_raw)
                .unwrap_or_else(|| precision.mm_to_raw(DEFAULT_DKS_TRAVEL_MM)),
            phases: dks_cfg
                .as_ref()
                .map(|c| std::array::from_fn(|i| c.bindings[i].phase_actions))
                .unwrap_or_default(),
        };

        // The Fn layer is a separate store, so it is always its own read.
        let fn_action = kb
            .and_then(|kb| {
                kb.active_fn_target()
                    .and_then(|target| kb.get_fn_keymatrix(target, 8))
                    .ok()
            })
            .and_then(|m| {
                m.get(key_index * 4..key_index * 4 + 4)
                    .map(|s| KeyAction::from_config_bytes([s[0], s[1], s[2], s[3]]))
            })
            .unwrap_or(KeyAction::Disabled);

        if let Some(ref triggers) = self.triggers {
            let modal = TriggerEditModal::new_per_key(
                key_index,
                triggers,
                self.precision,
                PerKeyEditPrefetch {
                    modtap_ms,
                    snaptap_partner,
                    key_choices,
                    slots,
                    fn_action,
                    dks,
                },
            );
            self.trigger_edit_modal = Some(modal);
            // Enable depth monitoring for the modal
            if !self.depth_monitoring {
                if let Some(ref keyboard) = self.keyboard {
                    let _ = keyboard.start_magnetism_report();
                }
                self.depth_monitoring = true;
            }
            let key_name = get_key_label(self, key_index);
            self.status_msg = format!(
                "Editing key {} ({}) - press it to see depth",
                key_index, key_name
            );
        } else {
            self.status_msg = "No trigger data loaded".to_string();
        }
    }

    /// Close trigger edit modal without saving
    pub(in crate::tui) fn close_trigger_edit_modal(&mut self) {
        self.trigger_edit_modal = None;
        self.status_msg = "Edit cancelled".to_string();
    }

    /// Save trigger edit modal changes
    pub(in crate::tui) fn save_trigger_edit_modal(&mut self) {
        let modal = match self.trigger_edit_modal.take() {
            Some(m) => m,
            None => return,
        };

        let Some(ref keyboard) = self.keyboard else {
            self.status_msg = "No keyboard connected".to_string();
            return;
        };

        let precision = self.precision;

        match modal.target {
            TriggerEditTarget::Global => {
                // The modal already holds raw units, so saving is not a conversion.
                let mut errors = Vec::new();

                if let Err(e) = keyboard.set_actuation_all(modal.actuation) {
                    errors.push(format!("actuation: {e}"));
                }
                if let Err(e) = keyboard.set_release_all(modal.release) {
                    errors.push(format!("release: {e}"));
                }
                if let Err(e) = keyboard.set_rt_press_all(modal.rt_press) {
                    errors.push(format!("rt_press: {e}"));
                }
                if let Err(e) = keyboard.set_rt_lift_all(modal.rt_lift) {
                    errors.push(format!("rt_lift: {e}"));
                }
                if let Err(e) = keyboard.set_top_deadzone_all(modal.top_dz) {
                    errors.push(format!("top_dz: {e}"));
                }
                if let Err(e) = keyboard.set_bottom_deadzone_all(modal.bottom_dz) {
                    errors.push(format!("bottom_dz: {e}"));
                }

                if errors.is_empty() {
                    self.status_msg = format!(
                        "Global triggers saved: act={} rel={}",
                        modal.actuation.format(precision),
                        modal.release.format(precision)
                    );
                    // Reload triggers to reflect changes
                    self.load_triggers();
                    if self.loading.key_mapping != LoadState::NotLoaded {
                        self.load_key_mapping();
                    }
                } else {
                    self.status_msg = format!("Errors: {}", errors.join(", "));
                }
            }
            TriggerEditTarget::PerKey { key_index } => {
                // Per-key uses the same u16 precision as the bulk table.
                let mode_byte = ModeByte::from_u8(modal.mode);
                let settings = KeyTriggerSettings {
                    key_index: key_index as u8,
                    actuation: modal.actuation.raw(),
                    deactuation: modal.release.raw(),
                    mode: mode_byte.base,
                    rapid_trigger: mode_byte.rapid_trigger,
                };

                match keyboard.set_key_trigger(&settings) {
                    Ok(()) => {
                        // Apply the mode-specific sub-configs alongside the base
                        // trigger. Mod-Tap time is only meaningful in Mod-Tap
                        // mode; Snap-Tap pairing only in Snap-Tap mode.
                        let key = key_index as u8;
                        let mut extra = Vec::new();
                        if mode_byte.base == KeyMode::ModTap
                            && let Err(e) = keyboard.set_modtap_time(key, modal.modtap_ms)
                        {
                            extra.push(format!("mt_time: {e}"));
                        }
                        if mode_byte.base == KeyMode::SnapTap {
                            let res = match modal.snaptap_partner {
                                Some(partner) => keyboard.set_snaptap_pair(key, partner),
                                None => keyboard.clear_snaptap(key),
                            };
                            if let Err(e) = res {
                                extra.push(format!("snaptap: {e}"));
                            }
                        }
                        if mode_byte.base == KeyMode::DynamicKeystroke {
                            // DKS owns all four keymatrix layers, so it writes them
                            // as one batch alongside the travel and phase data.
                            let bindings = std::array::from_fn(|i| {
                                DksBinding::from_packed_mode(
                                    DksBinding::pack_phase_actions(modal.dks_phases[i]),
                                    modal.slots[i].to_config_bytes(),
                                )
                            });
                            let config = DksConfig {
                                trigger_point_travel_raw: modal.dks_travel.raw(),
                                bindings,
                            };
                            if let Err(e) =
                                keyboard.set_dks_config(key, &config, Some(mode_byte.rapid_trigger))
                            {
                                extra.push(format!("dks: {e}"));
                            }
                        } else {
                            // Outside DKS only layers 0/1 and Fn are meaningful. Write
                            // just what changed; layer 0 must never go all-zero, since
                            // the base layer has no ROM fallback and would be silenced.
                            let changed = [
                                (Layer::Base, modal.slots[0], modal.slots_orig[0]),
                                (Layer::Layer1, modal.slots[1], modal.slots_orig[1]),
                                (Layer::Fn, modal.fn_action, modal.fn_action_orig),
                            ];
                            for (layer, now, before) in changed {
                                if now == before {
                                    continue;
                                }
                                let bytes = now.to_config_bytes();
                                if layer == Layer::Base && bytes == [0, 0, 0, 0] {
                                    continue;
                                }
                                let res = match layer.keymatrix_layer() {
                                    Some(km) => keyboard.set_keymatrix_config(
                                        keyboard.active_profile(),
                                        key,
                                        km,
                                        bytes,
                                        true,
                                    ),
                                    None => keyboard.set_fn_config(key, bytes),
                                };
                                if let Err(e) = res {
                                    extra.push(format!("output {layer}: {e}"));
                                }
                            }
                        }
                        let key_name = get_key_label(self, key_index);
                        self.status_msg = if extra.is_empty() {
                            format!(
                                "Key {} ({}) saved: act={} rel={} mode={}",
                                key_index,
                                key_name,
                                modal.actuation.format(precision),
                                modal.release.format(precision),
                                ModeByte::new(settings.mode, settings.rapid_trigger),
                            )
                        } else {
                            format!("Key {key_index} saved with errors: {}", extra.join(", "))
                        };
                        // Reload triggers to reflect changes
                        self.load_triggers();
                        if self.loading.key_mapping != LoadState::NotLoaded {
                            self.load_key_mapping();
                        }
                    }
                    Err(e) => {
                        self.status_msg = format!("Failed to save key {}: {}", key_index, e);
                    }
                }
            }
        }
    }

    /// Navigate to next valid key in layout view (Tab key)
    #[allow(dead_code)]
    pub(in crate::tui) fn layout_key_next(&mut self) {
        let max_key = self
            .triggers
            .as_ref()
            .map(|t| t.key_modes.len().saturating_sub(1))
            .unwrap_or(125);

        // Find next non-empty key
        for next in (self.trigger_selected_key + 1)..=max_key {
            if self.is_valid_key_position(next) {
                self.trigger_selected_key = next;
                return;
            }
        }
    }

    /// Navigate to previous valid key in layout view (Shift+Tab key)
    #[allow(dead_code)]
    pub(in crate::tui) fn layout_key_prev(&mut self) {
        if self.trigger_selected_key == 0 {
            return;
        }

        // Find previous non-empty key
        for prev in (0..self.trigger_selected_key).rev() {
            if self.is_valid_key_position(prev) {
                self.trigger_selected_key = prev;
                return;
            }
        }
    }

    /// Check if a matrix position has an active key
    pub(in crate::tui) fn is_valid_key_position(&self, pos: usize) -> bool {
        if pos >= self.matrix_size {
            return false;
        }
        let name = get_key_label(self, pos);
        !name.is_empty() && name != "?"
    }
}

// ============================================================================
// Render functions
// ============================================================================

/// Render trigger edit modal with depth chart
pub(in crate::tui) fn render_trigger_edit_modal(f: &mut Frame, app: &App, area: Rect) {
    let modal = match &app.trigger_edit_modal {
        Some(m) => m,
        None => return,
    };

    // Calculate popup size (70% width, 80% height)
    let popup_width = (area.width as f32 * 0.70).min(80.0) as u16;
    let popup_height = (area.height as f32 * 0.85).min(38.0) as u16;
    let popup_x = (area.width.saturating_sub(popup_width)) / 2;
    let popup_y = (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

    // Clear the area behind the popup
    f.render_widget(Clear, popup_area);

    // Title based on target
    let title = match modal.target {
        TriggerEditTarget::Global => " Edit Global Trigger Settings ".to_string(),
        TriggerEditTarget::PerKey { key_index } => {
            let key_name = get_key_label(app, key_index);
            format!(" Edit Key {} ({}) ", key_index, key_name)
        }
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(title)
        .title_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );

    let inner = block.inner(popup_area);
    f.render_widget(block, popup_area);

    // Split into chart area and fields area
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),    // Depth chart
            Constraint::Min(18),   // Fields (per-key modal has many rows)
            Constraint::Length(2), // Help line
        ])
        .split(inner);

    // Render depth chart
    render_modal_depth_chart(f, modal, app, chunks[0]);

    // Render editable fields
    render_modal_fields(f, modal, chunks[1]);

    // Render help line — the output picker rebinds Tab, so say so while it is open.
    let help_text = if modal.output_picker.is_some() {
        "type: filter | ↑↓: pick | Tab: add to chord | Enter: confirm | Esc: cancel"
    } else {
        "Tab/↑↓: field | ←/→: adjust | Enter: open/activate | ^S: save | Esc: cancel"
    };
    let help = Paragraph::new(help_text)
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[2]);

    // Overlay a picker if open. The renderer only has `&App`, so clone the small
    // picker to satisfy the stateful-widget `&mut` requirement.
    if let Some(picker) = &modal.mode_picker {
        picker.clone().render(f, popup_area);
    } else if let Some(picker) = &modal.key_picker {
        picker.clone().render(f, popup_area);
    } else if let Some(picker) = &modal.output_picker {
        picker.clone().render(f, popup_area);
    } else if let Some((_, picker)) = &modal.dks_action_picker {
        picker.clone().render(f, popup_area);
    }
}

/// Render the depth chart within the modal
fn render_modal_depth_chart(f: &mut Frame, modal: &TriggerEditModal, app: &App, area: Rect) {
    use ratatui::widgets::{Axis, Chart, Dataset, GraphType};

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Key Depth ")
        .title_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Build depth data from history
    let depth_data: Vec<(f64, f64)> = modal
        .depth_history
        .iter()
        .enumerate()
        .map(|(i, &d)| (i as f64, d as f64))
        .collect();

    // Also get current depth for filtered key or max active key
    let current_depth = if let Some(key_idx) = modal.depth_filter {
        app.key_depths.get(key_idx).copied().unwrap_or(0.0)
    } else {
        // Show max depth across all active keys
        app.key_depths.iter().copied().fold(0.0f32, |a, b| a.max(b))
    };

    // Create threshold lines
    let max_samples = 100.0;
    let mm = |d: TravelDepth| modal.precision.raw_to_mm(d.raw());
    let level = |y: f64| -> Vec<(f64, f64)> { vec![(0.0, y), (max_samples, y)] };
    let actuation_line = level(mm(modal.actuation));
    let release_line = level(mm(modal.release));
    let top_dz_line = level(mm(modal.top_dz));
    let bottom_dz_line = level(4.0 - mm(modal.bottom_dz));

    let mut datasets = vec![
        // Depth trace
        Dataset::default()
            .name("Depth")
            .marker(ratatui::symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::White))
            .data(&depth_data),
        // Actuation threshold
        Dataset::default()
            .name("Act")
            .marker(ratatui::symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Yellow))
            .data(&actuation_line),
        // Release threshold
        Dataset::default()
            .name("Rel")
            .marker(ratatui::symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Cyan))
            .data(&release_line),
    ];

    // Only show deadzone lines if non-zero
    if modal.top_dz.raw() > 0 {
        datasets.push(
            Dataset::default()
                .name("TopDZ")
                .marker(ratatui::symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(Color::Green))
                .data(&top_dz_line),
        );
    }
    if modal.bottom_dz.raw() > 0 {
        datasets.push(
            Dataset::default()
                .name("BotDZ")
                .marker(ratatui::symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(Color::Red))
                .data(&bottom_dz_line),
        );
    }

    // Current depth indicator
    let depth_str = format!("{:.2}mm", current_depth);

    let chart = Chart::new(datasets)
        .x_axis(
            Axis::default()
                .title("Time")
                .style(Style::default().fg(Color::DarkGray))
                .bounds([0.0, max_samples]),
        )
        .y_axis(
            Axis::default()
                .title(depth_str)
                .style(Style::default().fg(Color::DarkGray))
                .labels(vec![
                    Span::raw("0"),
                    Span::raw("1"),
                    Span::raw("2"),
                    Span::raw("3"),
                    Span::raw("4"),
                ])
                .bounds([0.0, 4.0]),
        );

    f.render_widget(chart, inner);
}

/// Render the editable fields in the modal using spinner style
fn render_modal_fields(f: &mut Frame, modal: &TriggerEditModal, area: Rect) {
    let fields = modal.visible_fields();
    let mut lines: Vec<Line> = Vec::new();

    for (i, field) in fields.iter().enumerate() {
        let is_selected = i == modal.field_index;

        // The Save button is a row, not a setting — render it as a button.
        if *field == TriggerField::Save {
            let style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Green)
            };
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("[ Save ]", style),
            ]));
            continue;
        }

        let label = format!("{:12}", field.label_for(modal.mode));

        // Get value and unit from spinner config, or special handling for the
        // Mode (base name), Rapid-Trigger (on/off) and Snap-Tap partner fields.
        let (value, unit) = match field {
            TriggerField::Mode => (KeyMode::from_u8(modal.mode).label().to_string(), ""),
            TriggerField::RapidTrigger => {
                let on = modal.mode & ModeByte::RT_FLAG != 0;
                ((if on { "On" } else { "Off" }).to_string(), "")
            }
            TriggerField::OutputLayer => (modal.output_layer_name().to_string(), ""),
            TriggerField::Output => (modal.output_action().to_string(), ""),
            TriggerField::SnapTapPartner => {
                let label = match modal.snaptap_partner {
                    Some(idx) => modal
                        .key_choices
                        .iter()
                        .find(|(_, i)| *i == idx)
                        .map(|(l, _)| l.clone())
                        .unwrap_or_else(|| format!("key {idx}")),
                    None => "(none)".to_string(),
                };
                (label, "")
            }
            TriggerField::DksBinding => (format!("{} / 4", modal.dks_binding_index + 1), ""),
            TriggerField::DksBindingKey => {
                let label = match modal.slots[modal.dks_binding_index] {
                    KeyAction::Disabled => "(none)".to_string(),
                    action => action.to_string(),
                };
                (label, "")
            }
            TriggerField::DksAct0
            | TriggerField::DksAct1
            | TriggerField::DksAct2
            | TriggerField::DksAct3 => {
                let idx = field.dks_action_index().unwrap();
                (
                    modal.dks_phases[modal.dks_binding_index][idx].to_string(),
                    "",
                )
            }
            _ => {
                let config = field
                    .spinner_config(modal.precision)
                    .expect("spinner field");
                (config.format(modal.value_of(*field)), config.unit())
            }
        };

        // Spinner-style display: < value > when selected, just value when not
        let display_value = if is_selected {
            format!("< {} >", value)
        } else {
            format!("  {}  ", value)
        };

        let label_style = Style::default().fg(Color::Gray);
        let value_style = if is_selected {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let unit_style = Style::default().fg(Color::DarkGray);

        let mut spans = vec![
            Span::raw("  "),
            Span::styled(label, label_style),
            Span::styled(display_value, value_style),
        ];
        if !unit.is_empty() {
            spans.push(Span::styled(format!(" {}", unit), unit_style));
        }

        lines.push(Line::from(spans));
    }

    // Add help text at bottom
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  ←/→", Style::default().fg(Color::Cyan)),
        Span::raw(" adjust  "),
        Span::styled("Shift", Style::default().fg(Color::Cyan)),
        Span::raw(" coarse  "),
        Span::styled("↑/↓", Style::default().fg(Color::Cyan)),
        Span::raw(" select  "),
        Span::styled("Enter", Style::default().fg(Color::Green)),
        Span::raw(" open  "),
        Span::styled("^S", Style::default().fg(Color::Green)),
        Span::raw(" save  "),
        Span::styled("Esc", Style::default().fg(Color::Red)),
        Span::raw(" cancel"),
    ]));

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modal() -> TriggerEditModal {
        TriggerEditModal::new_global(&TriggerSettings::default(), Precision::default())
    }

    /// Nothing staged: Enter takes whatever is highlighted, chord or not.
    #[test]
    fn picked_action_without_staging_is_the_highlighted_row() {
        let m = modal();
        assert_eq!(
            m.picked_action(KeyAction::Key(HidUsage::new(0x06))),
            KeyAction::Key(HidUsage::new(0x06))
        );
        assert_eq!(
            m.picked_action(KeyAction::Consumer(0x00E9)),
            KeyAction::Consumer(0x00E9)
        );
    }

    /// Staged usages combine with the highlighted key, in staging order.
    #[test]
    fn picked_action_appends_highlighted_key_to_staged_chord() {
        let mut m = modal();
        m.chord_buf = vec![HidUsage::new(0xE0)];
        assert_eq!(
            m.picked_action(KeyAction::Key(HidUsage::new(0x06))),
            KeyAction::Combo {
                keys: [HidUsage::new(0xE0), HidUsage::new(0x06), HidUsage::new(0)]
            }
        );
        // Confirming on a key that is already staged must not duplicate it.
        m.chord_buf = vec![HidUsage::new(0xE0), HidUsage::new(0x06)];
        assert_eq!(
            m.picked_action(KeyAction::Key(HidUsage::new(0x06))),
            KeyAction::Combo {
                keys: [HidUsage::new(0xE0), HidUsage::new(0x06), HidUsage::new(0)]
            }
        );
    }

    /// A media key can't join a chord — it occupies the whole entry — so it is
    /// dropped rather than corrupting the staged usages.
    #[test]
    fn picked_action_ignores_non_key_highlight_while_staging() {
        let mut m = modal();
        m.chord_buf = vec![HidUsage::new(0xE0), HidUsage::new(0x06)];
        assert_eq!(
            m.picked_action(KeyAction::Consumer(0x00E9)),
            KeyAction::Combo {
                keys: [HidUsage::new(0xE0), HidUsage::new(0x06), HidUsage::new(0)]
            }
        );
    }
}
