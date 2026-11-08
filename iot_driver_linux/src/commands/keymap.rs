//! Key remapping command handlers.

use super::CommandResult;
use iot_driver::key_action::KeyAction;
use iot_driver::keymap::{self, KeyTable, Layer};
use iot_driver::keymatrix_view::{self, ListOptions};
use iot_driver::protocol::hid;
use monsgeek_keyboard::KeyboardInterface;
use monsgeek_transport::protocol::{KeymatrixLayer, Profile};

/// Remap a key.
///
/// `from` names a key of the connected board and can carry a layer prefix:
/// `"Fn+RCtrl"`, `"L1+A"`, `"42"`, `"#42"`. A prefix takes precedence over the
/// `--layer` flag.
pub fn remap(keyboard: &KeyboardInterface, from: &str, to: &str, layer: Layer) -> CommandResult {
    let table = KeyTable::from_device(keyboard);
    let (prefix, index) = match table.parse_ref(from) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("{msg}");
            return Ok(());
        }
    };
    let layer = prefix.unwrap_or(layer);

    let action: KeyAction = match to.parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Invalid target key: {e}");
            return Ok(());
        }
    };

    let label = table.ref_label(index, layer);
    println!(
        "Remapping {label} (index {}) to {action} on {}...",
        index.get(),
        layer.name()
    );
    match keymap::set_key(keyboard, index, layer, &action) {
        Ok(()) => println!("{label} remapped to {action}"),
        Err(e) => eprintln!("Failed to remap key: {e}"),
    }
    Ok(())
}

/// Reset a key to default.
///
/// `key` names a key of the connected board and can carry a layer prefix:
/// `"Fn+RCtrl"`, `"L1+A"`.
pub fn reset_key(keyboard: &KeyboardInterface, key: &str, layer: Layer) -> CommandResult {
    let table = KeyTable::from_device(keyboard);
    let (prefix, index) = match table.parse_ref(key) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("{msg}");
            return Ok(());
        }
    };
    let layer = prefix.unwrap_or(layer);

    let label = table.ref_label(index, layer);
    println!(
        "Resetting {label} (index {}) on {}...",
        index.get(),
        layer.name()
    );
    match keymap::reset_key(keyboard, index, layer) {
        Ok(()) => println!("{label} reset to default"),
        Err(e) => eprintln!("Failed to reset key: {e}"),
    }
    Ok(())
}

/// Swap two keys within a profile.
///
/// Only ever touches keymatrix layer 0: `swap_keys` writes through
/// `set_keymatrix(profile, .., layer: 0)`, so the read must match.
pub fn swap(
    keyboard: &KeyboardInterface,
    key1: &str,
    key2: &str,
    profile: Profile,
) -> CommandResult {
    let table = KeyTable::from_device(keyboard);
    let (key_a, key_b) = match (table.resolve(key1), table.resolve(key2)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(msg), _) | (_, Err(msg)) => {
            eprintln!("{msg}");
            return Ok(());
        }
    };

    match keyboard.get_keymatrix(profile, KeymatrixLayer::BASE, 8) {
        Ok(data) => {
            // The keymatrix entry's usage sits at byte 2 of the 4-byte slot.
            let usage_at = |pos: monsgeek_transport::protocol::MatrixPos| {
                data.get(usize::from(pos) * 4 + 2)
                    .copied()
                    .map(monsgeek_transport::protocol::HidUsage::new)
                    .unwrap_or(monsgeek_transport::protocol::HidUsage::NONE)
            };
            let (code_a, code_b) = (usage_at(key_a), usage_at(key_b));

            let name_a = table.label(key_a);
            let name_b = table.label(key_b);
            let action_a = hid::key_name(code_a);
            let action_b = hid::key_name(code_b);
            println!("Swapping {name_a} ({action_a}) <-> {name_b} ({action_b})...");

            match keyboard.swap_keys(
                profile,
                key_a.get(),
                code_a.get(),
                key_b.get(),
                code_b.get(),
            ) {
                Ok(()) => println!("Keys swapped successfully"),
                Err(e) => eprintln!("Failed to swap keys: {e}"),
            }
        }
        Err(e) => eprintln!("Failed to read current key mappings: {e}"),
    }
    Ok(())
}

/// Show per-key bindings across layers.
pub fn keymatrix(
    keyboard: &KeyboardInterface,
    layers: &[crate::cli::LayerArg],
    unset: bool,
    keys: &[iot_driver::keyclass::KeySelector],
    sys: Option<crate::cli::SysArg>,
    raw: bool,
) -> CommandResult {
    // An empty selector list means "every key", which is what resolve() returns,
    // but pass it through as empty so the renderer skips the per-key filter.
    // Resolved before the bulk reads so a typo fails fast.
    let selected = if keys.is_empty() {
        Vec::new()
    } else {
        match iot_driver::keyclass::KeySelector::resolve(keys, &KeyTable::from_device(keyboard)) {
            Ok(selected) => selected,
            Err(e) => {
                eprintln!("{e}");
                return Ok(());
            }
        }
    };
    let rows = match keymap::load_key_rows(keyboard, sys.map(|s| s.wire())) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("Failed to read key matrix: {e}");
            return Ok(());
        }
    };
    let opts = ListOptions {
        layers: layers.iter().map(|&l| l.into()).collect(),
        keys: selected,
        show_unset: unset,
        raw,
        precision: keyboard.get_precision().unwrap_or_default(),
    };
    print!("{}", keymatrix_view::render(&rows, &opts));
    Ok(())
}
