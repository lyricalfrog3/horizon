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

use hfc_runtime::bridge::{FabricModMetadata, RegisteredItem};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The most bytes copied out of one jar.
const MAX_COPY: u64 = 256 << 20;

/// Writes the generated content mods under `out` and returns their folders.
pub(in super::super) fn fabric_content_mods(out: &Path) -> Vec<PathBuf> {
    let (items, mods) = match super::get_fabric_bridge().lock() {
        Ok(bridge) => (bridge.registered_items(), bridge.loaded_mods().to_vec()),
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
        match write_mod(out, &namespace, &items, &mods) {
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
fn carried(name: &str, namespace: &str) -> bool {
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
        "assets" => ns == namespace,
        "data" if ns == namespace => !rest.starts_with("horizon/"),
        "data" => matches!(ns, "minecraft" | "c") && rest.starts_with("tags/"),
        _ => false,
    }
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
    let mut copied = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().map_err(|e| e.to_string())?.into_owned();
        if entry.is_dir() || !carried(&name, namespace) || name.contains("..") {
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
        assert!(carried(
            "assets/emerald-tools-justfatlard/models/item/emerald_axe.json",
            ns
        ));
        assert!(carried(
            "data/emerald-tools-justfatlard/recipe/emerald_axe.json",
            ns
        ));
        assert!(carried("data/minecraft/tags/item/pickaxes.json", ns));
        assert!(!carried("data/minecraft/advancement/story/x.json", ns));
        assert!(!carried("assets/minecraft/textures/x.png", ns));
        assert!(!carried(
            "data/emerald-tools-justfatlard/horizon/item/x.json",
            ns
        ));
        assert!(!carried("fabric.mod.json", ns));
    }
}
