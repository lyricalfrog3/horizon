//! Content mods for the items Fabric mods register in code.
//!
//! A Fabric mod registers its items from its entrypoint
//! (`Registry.register(BuiltInRegistries.ITEM, ...)`); the runtime runs that
//! code and reports each item as a Horizon item declaration: a vanilla
//! template and the data components the mod's `Item.Properties` set
//! ([`hfc_runtime::bridge::RegisteredItem`]). Here those declarations, with
//! the jar's own assets, data and tags in the same namespace, become one
//! content mod folder per namespace, which the content loader reads like
//! any other folder mod. The folders are rebuilt at every start.
//!
//! Where the mod puts its items in creative tabs (Fabric's
//! `CreativeModeTabEvents.modifyOutputEvent`) becomes the mod's
//! `tab_entries` file per tab.

use hfc_runtime::bridge::{CreativeTabAddition, FabricModMetadata, RegisteredItem, TabPlacement};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

/// The most bytes copied out of one jar.
const MAX_COPY: u64 = 256 << 20;

/// Writes the generated content mods under `out` and returns their folders.
pub(in super::super) fn fabric_content_mods(out: &Path) -> Vec<PathBuf> {
    let (items, tabs, mods) = match super::get_fabric_bridge().lock() {
        Ok(bridge) => (
            bridge.registered_items(),
            bridge.creative_tab_additions(),
            bridge.loaded_mods().to_vec(),
        ),
        Err(_) => return Vec::new(),
    };
    let _ = std::fs::remove_dir_all(out);
    let mut by_namespace: BTreeMap<String, Vec<RegisteredItem>> = BTreeMap::new();
    for item in items {
        if let Some((namespace, _)) = item.id.split_once(':') {
            by_namespace
                .entry(namespace.to_owned())
                .or_default()
                .push(item);
        }
    }
    let mut folders = Vec::new();
    for (namespace, items) in by_namespace {
        // The tab placements of this namespace's items, in the mod's order.
        let tabs: Vec<&CreativeTabAddition> = tabs
            .iter()
            .filter(|t| {
                t.item
                    .split_once(':')
                    .is_some_and(|(ns, _)| ns == namespace)
            })
            .collect();
        match write_mod(out, &namespace, &items, &tabs, &mods) {
            Ok(folder) => {
                log::info!(
                    "Fabric: {} item(s) registered in {namespace}: content mod {}",
                    items.len(),
                    folder.display()
                );
                folders.push(folder);
            }
            Err(e) => log::warn!("Fabric: items of {namespace} not loaded: {e}"),
        }
    }
    folders
}

/// A Horizon mod id (`[a-z0-9_]`, at most 64) for a resource namespace,
/// which may also hold `-` and `.`.
fn mod_id(namespace: &str) -> String {
    let id: String = namespace
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if id.is_empty() { "fabric".into() } else { id }
}

/// Whether a jar file belongs in the content mod for `namespace`: its own
/// assets and data, and its additions to vanilla and convention tags (what a
/// content mod may carry). Declarations are written from the runtime, not
/// copied.
fn carried(name: &str, namespace: &str, referenced: &BTreeSet<String>) -> bool {
    let mut parts = name.splitn(3, '/');
    let (top, ns, rest) = (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
    );
    if name.ends_with('/') || rest.is_empty() {
        return false;
    }
    match top {
        // The mod's own assets, and the worn-armour files its items name in
        // another namespace (vanilla's data-driven equipment lives under
        // `minecraft`).
        "assets" => ns == namespace || referenced.contains(name),
        "data" if ns == namespace => !rest.starts_with("horizon/"),
        "data" => matches!(ns, "minecraft" | "c") && rest.starts_with("tags/"),
        _ => false,
    }
}

/// The equipment assets the items' `minecraft:equippable` `asset_id`s name, and the
/// textures those assets' layers name, as jar paths outside `namespace` that a content
/// mod may carry: only `minecraft` (Horizon's fence lets a mod add equipment there
/// without replacing vanilla's). `read` gives a jar file's bytes. Files in the mod's own
/// namespace are carried anyway.
fn referenced_equipment(
    items: &[RegisteredItem],
    namespace: &str,
    mut read: impl FnMut(&str) -> Option<Vec<u8>>,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let assets: BTreeSet<&str> = items
        .iter()
        .filter_map(|i| {
            i.components
                .get("minecraft:equippable")?
                .get("asset_id")?
                .as_str()
        })
        .collect();
    for asset in assets {
        let (ns, path) = asset.split_once(':').unwrap_or(("minecraft", asset));
        if !matches!(ns, "minecraft") && ns != namespace {
            continue;
        }
        let file = format!("assets/{ns}/equipment/{path}.json");
        let Some(doc) =
            read(&file).and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        else {
            continue;
        };
        out.insert(file);
        let Some(layers) = doc.get("layers").and_then(|l| l.as_object()) else {
            continue;
        };
        for (kind, list) in layers {
            for layer in list.as_array().into_iter().flatten() {
                // A layer without a texture uses the asset's own id.
                let texture = layer
                    .get("texture")
                    .and_then(|t| t.as_str())
                    .unwrap_or(asset);
                let (tns, tpath) = texture.split_once(':').unwrap_or(("minecraft", texture));
                if tns == "minecraft" || tns == namespace {
                    out.insert(format!(
                        "assets/{tns}/textures/entity/equipment/{kind}/{tpath}.png"
                    ));
                }
            }
        }
    }
    out
}

/// The loaded mod whose jar holds `namespace`: the mod of that id, else one
/// whose jar has files there.
fn owner<'a>(namespace: &str, mods: &'a [FabricModMetadata]) -> Option<&'a FabricModMetadata> {
    let outer = |m: &&FabricModMetadata| !m.is_nested && m.jar.is_some();
    mods.iter()
        .filter(outer)
        .find(|m| m.id == namespace)
        .or_else(|| {
            mods.iter().filter(outer).find(|m| {
                let Some(jar) = &m.jar else { return false };
                let Ok(file) = std::fs::File::open(jar) else {
                    return false;
                };
                let Ok(zip) = zip::ZipArchive::new(file) else {
                    return false;
                };
                zip.file_names().flatten().any(|name| {
                    name.strip_prefix("assets/")
                        .or_else(|| name.strip_prefix("data/"))
                        .is_some_and(|rest| rest.starts_with(&format!("{namespace}/")))
                })
            })
        })
}

fn write_mod(
    out: &Path,
    namespace: &str,
    items: &[RegisteredItem],
    tabs: &[&CreativeTabAddition],
    mods: &[FabricModMetadata],
) -> Result<PathBuf, String> {
    let owner = owner(namespace, mods).ok_or("no loaded jar holds this namespace")?;
    let id = mod_id(namespace);
    let folder = out.join(&id);
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let quote = |text: &str| serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
    let mut manifest = format!(
        "id = {}\nname = {}\nversion = {}\ndescription = {}\napi = \"0.2\"\ncapabilities = [\"registries\"]\n",
        quote(&id),
        quote(&owner.name),
        quote(&owner.version),
        quote(&format!(
            "Items the Fabric mod {} registers in code",
            owner.id
        )),
    );
    if id != namespace {
        manifest.push_str(&format!("namespaces = [{}]\n", quote(namespace)));
    }
    std::fs::write(folder.join("horizon-mod.toml"), manifest).map_err(|e| e.to_string())?;

    let jar = owner.jar.as_deref().ok_or("the mod has no jar")?;
    let file = std::fs::File::open(jar).map_err(|e| format!("{jar}: {e}"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("{jar}: {e}"))?;
    let referenced = referenced_equipment(items, namespace, |name| {
        let mut bytes = Vec::new();
        zip.by_name(name).ok()?.read_to_end(&mut bytes).ok()?;
        Some(bytes)
    });
    let mut copied = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().map_err(|e| e.to_string())?.into_owned();
        if entry.is_dir() || !carried(&name, namespace, &referenced) || name.contains("..") {
            continue;
        }
        copied += entry.size();
        if copied > MAX_COPY {
            return Err(format!("{jar}: over {} MiB of content", MAX_COPY >> 20));
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| format!("{name}: {e}"))?;
        let path = folder.join(&name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, bytes).map_err(|e| format!("{name}: {e}"))?;
    }

    for item in items {
        let Some((_, path)) = item.id.split_once(':') else {
            continue;
        };
        let mut file = serde_json::Map::new();
        file.insert("template".into(), item.template.clone().into());
        if !item.components.is_empty() {
            file.insert(
                "components".into(),
                serde_json::Value::Object(item.components.clone()),
            );
        }
        if let Some(size) = item.max_stack_size {
            file.insert("max_stack_size".into(), size.into());
        }
        if let Some(rarity) = &item.rarity {
            file.insert("rarity".into(), rarity.clone().into());
        }
        let target = folder.join(format!("data/{namespace}/horizon/item/{path}.json"));
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_vec_pretty(&serde_json::Value::Object(file))
            .map_err(|e| e.to_string())?;
        std::fs::write(&target, text).map_err(|e| e.to_string())?;
    }
    // One `tab_entries` file per tab, entries in the order the mod made them.
    let mut by_tab: BTreeMap<&str, Vec<serde_json::Value>> = BTreeMap::new();
    for addition in tabs {
        let mut entry = serde_json::Map::new();
        entry.insert("item".into(), addition.item.clone().into());
        match &addition.placement {
            TabPlacement::End => {}
            TabPlacement::Start => {
                entry.insert("start".into(), true.into());
            }
            TabPlacement::After(anchor) => {
                entry.insert("after".into(), anchor.clone().into());
            }
            TabPlacement::Before(anchor) => {
                entry.insert("before".into(), anchor.clone().into());
            }
        }
        by_tab
            .entry(addition.tab.as_str())
            .or_default()
            .push(serde_json::Value::Object(entry));
    }
    for (tab, entries) in by_tab {
        let name = mod_id(&tab.replace(':', "_"));
        let target = folder.join(format!("data/{namespace}/horizon/tab_entries/{name}.json"));
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let file = serde_json::json!({ "tab": tab, "entries": entries });
        let text = serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())?;
        std::fs::write(&target, text).map_err(|e| e.to_string())?;
    }
    Ok(folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_namespace_becomes_a_horizon_mod_id() {
        assert_eq!(
            mod_id("emerald-tools-justfatlard"),
            "emerald_tools_justfatlard"
        );
        assert_eq!(mod_id("ruby"), "ruby");
        assert_eq!(mod_id("a.b"), "a_b");
    }

    #[test]
    fn only_the_namespace_and_shared_tags_are_carried() {
        let ns = "emerald-tools-justfatlard";
        let none = BTreeSet::new();
        assert!(carried(
            "assets/emerald-tools-justfatlard/models/item/emerald_axe.json",
            ns,
            &none
        ));
        assert!(carried(
            "data/emerald-tools-justfatlard/recipe/emerald_axe.json",
            ns,
            &none
        ));
        assert!(carried("data/minecraft/tags/item/pickaxes.json", ns, &none));
        assert!(!carried(
            "data/minecraft/advancement/story/x.json",
            ns,
            &none
        ));
        assert!(!carried("assets/minecraft/textures/x.png", ns, &none));
        assert!(!carried(
            "data/emerald-tools-justfatlard/horizon/item/x.json",
            ns,
            &none
        ));
        assert!(!carried("fabric.mod.json", ns, &none));
    }

    fn armor_item(id: &str, asset: &str) -> RegisteredItem {
        let mut components = serde_json::Map::new();
        components.insert(
            "minecraft:equippable".into(),
            serde_json::json!({"slot": "head", "asset_id": asset}),
        );
        RegisteredItem {
            id: id.into(),
            template: "minecraft:iron_helmet".into(),
            components,
            max_stack_size: None,
            rarity: None,
        }
    }

    #[test]
    fn worn_armour_assets_in_minecraft_are_carried_with_their_textures() {
        let ns = "gems";
        let items = [armor_item("gems:ruby_helmet", "minecraft:ruby")];
        let jar = |name: &str| match name {
            "assets/minecraft/equipment/ruby.json" => Some(
                br#"{"layers": {
                    "humanoid": [{"texture": "minecraft:ruby_armor"}],
                    "humanoid_leggings": [{"texture": "minecraft:ruby_armor"}],
                    "wings": [{"texture": "gems:ruby_wings"}, {"texture": "other:x"}]}}"#
                    .to_vec(),
            ),
            _ => None,
        };
        let referenced = referenced_equipment(&items, ns, jar);
        let expected = [
            "assets/gems/textures/entity/equipment/wings/ruby_wings.png",
            "assets/minecraft/equipment/ruby.json",
            "assets/minecraft/textures/entity/equipment/humanoid/ruby_armor.png",
            "assets/minecraft/textures/entity/equipment/humanoid_leggings/ruby_armor.png",
        ];
        assert_eq!(
            referenced.iter().map(String::as_str).collect::<Vec<_>>(),
            expected
        );
        for name in expected {
            assert!(carried(name, ns, &referenced), "{name}");
        }
        // Only what the items name: vanilla's other files and models stay out.
        assert!(!carried(
            "assets/minecraft/equipment/iron.json",
            ns,
            &referenced
        ));
        assert!(!carried(
            "assets/minecraft/textures/block/stone.png",
            ns,
            &referenced
        ));
        // An asset the jar does not hold, or another mod's namespace, names nothing.
        assert!(referenced_equipment(&items, ns, |_| None).is_empty());
        let foreign = [armor_item("gems:a", "other:ruby")];
        assert!(referenced_equipment(&foreign, ns, jar).is_empty());
    }
}
