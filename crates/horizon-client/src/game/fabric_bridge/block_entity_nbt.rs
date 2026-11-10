//! A Fabric block entity's saved state as NBT.
//!
//! The runtime records what a block entity's own `saveAdditional` wrote as a
//! flattened `ValueOutput` tree (`hfc_runtime::value_io_builtins::flat_values`:
//! `"CookTime"`, `"Inventory/Slot0"`). The host keeps a block entity's saved
//! compound in `ModBlockEntityFields.nbt`, so this maps between the two, under
//! one key: a Fabric block entity's own fields never collide with a native
//! mod's, and a value that is not a primitive is written as the string it
//! would be.

use hfc_runtime::value_io_builtins::StoredValue;
use horizon_world::nbt::{Compound, JavaString, Tag};

/// The compound key a Fabric block entity's own saved tree lives under.
pub const KEY: &str = "HorizonFabric";

/// The tree as nested compounds: `a/b` becomes `a: {b: ...}`.
pub fn to_nbt(values: &[(String, StoredValue)]) -> Compound {
    let mut root = Compound::new();
    for (path, value) in values {
        let parts: Vec<&str> = path.split('/').collect();
        let mut current = &mut root;
        for part in &parts[..parts.len() - 1] {
            let entry = current
                .entry(JavaString::from(*part))
                .or_insert_with(|| Tag::Compound(Compound::new()));
            let Tag::Compound(inner) = entry else {
                // A value and a child with the same name: the child wins, as
                // the runtime's own tree cannot hold both either.
                *entry = Tag::Compound(Compound::new());
                let Tag::Compound(inner) = entry else {
                    unreachable!("just assigned a compound")
                };
                current = inner;
                continue;
            };
            current = inner;
        }
        current.insert(JavaString::from(parts[parts.len() - 1]), tag_of(value));
    }
    root
}

fn tag_of(value: &StoredValue) -> Tag {
    match value {
        StoredValue::Int(v) => Tag::Int(*v),
        StoredValue::Long(v) => Tag::Long(*v),
        StoredValue::Byte(v) => Tag::Byte(*v),
        StoredValue::Short(v) => Tag::Short(*v),
        StoredValue::Float(v) => Tag::Float(v.to_bits()),
        StoredValue::Double(v) => Tag::Double(v.to_bits()),
        StoredValue::Bool(v) => Tag::Byte(i8::from(*v)),
        StoredValue::Text(v) => Tag::String(v.as_str().into()),
        StoredValue::IntArray(v) => Tag::IntArray(v.clone()),
        StoredValue::Child(_) => Tag::Compound(Compound::new()),
    }
}

/// The tree a compound holds, flattened the way the runtime's input takes it.
pub fn from_nbt(compound: &Compound) -> Vec<(String, StoredValue)> {
    fn walk(compound: &Compound, prefix: &str, out: &mut Vec<(String, StoredValue)>, depth: usize) {
        if depth > 16 {
            return;
        }
        for (key, tag) in compound {
            let name = format!("{prefix}{}", key.to_string_lossy());
            if let Tag::Compound(inner) = tag {
                walk(inner, &format!("{name}/"), out, depth + 1);
                continue;
            }
            let value = match tag {
                Tag::Int(v) => StoredValue::Int(*v),
                Tag::Long(v) => StoredValue::Long(*v),
                Tag::Byte(v) => StoredValue::Byte(*v),
                Tag::Short(v) => StoredValue::Short(*v),
                Tag::Float(v) => StoredValue::Float(f32::from_bits(*v)),
                Tag::Double(v) => StoredValue::Double(f64::from_bits(*v)),
                Tag::String(v) => StoredValue::Text(v.to_string_lossy()),
                Tag::IntArray(v) => StoredValue::IntArray(v.clone()),
                _ => continue,
            };
            out.push((name, value));
        }
    }
    let mut out = Vec::new();
    walk(compound, "", &mut out, 0);
    out
}
