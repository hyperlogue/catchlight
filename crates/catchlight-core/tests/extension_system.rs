#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a test aborts on a broken fixture"
)]

//! Typed extensions: decoding, versioning, reference maintenance, checking
//! and addon merging. The registry is process-wide, so every extension type
//! here has a key of its own.

use std::sync::Arc;

use catchlight_core::extension::{
    DecodeError, Extension, ExtensionError, IdRef, MergeRefused, MigrationError, Official, Payload,
    RawExtension, Receipt, RegisterError, RemovedRef, Unread, Warning, codes, register,
    register_official, sealed,
};
use catchlight_core::formats::clm::{ClmIndices, ClmMesh, TextureAlpha, TextureEncoding};
use catchlight_core::{
    ExtensionKey, ExtensionValue, InstallError, Model, ModelNode, ModelNodeKind, ModelParam,
    ModelPart, ModelTexture, Name, NodeId, ParamId, ReadOptions, Required, SlotId, TexId,
};
use serde_json::json;

/// Notes pinned to Ids: payload `{"notes": [[ref index, text], ...]}`.
#[derive(Clone, Debug, Default, PartialEq)]
struct Notes {
    refs: Vec<IdRef>,
    notes: Vec<(u32, String)>,
}

impl Notes {
    fn texts(&self) -> Vec<&str> {
        self.notes.iter().map(|(_, t)| t.as_str()).collect()
    }

    fn add(&mut self, r: IdRef, text: &str) {
        let at = self.refs.iter().position(|x| x == &r).unwrap_or_else(|| {
            self.refs.push(r);
            self.refs.len() - 1
        });
        self.notes.push((at as u32, text.to_string()));
    }

    fn decode(raw: RawExtension) -> Result<Self, DecodeError> {
        let json = raw
            .payload
            .json()
            .ok_or_else(|| DecodeError::new("not json"))?;
        let notes = serde_json::from_value(json["notes"].clone()).map_err(DecodeError::new)?;
        Ok(Self {
            refs: raw.refs,
            notes,
        })
    }

    fn encode(&self) -> RawExtension {
        RawExtension {
            version: 2,
            refs: self.refs.clone(),
            payload: Payload::Json(json!({ "notes": self.notes })),
        }
    }

    /// 0 -> 1 renames `old` to `pairs`; 1 -> 2 renames `pairs` to `notes`.
    fn migrate(from: u32, mut raw: RawExtension) -> Result<RawExtension, MigrationError> {
        let (was, now) = match from {
            0 => ("old", "pairs"),
            1 => ("pairs", "notes"),
            _ => return Err(MigrationError::Unknown(from)),
        };
        let object = raw
            .payload
            .json_mut()?
            .as_object_mut()
            .ok_or_else(|| MigrationError::Failed("not an object".into()))?;
        let value = object
            .remove(was)
            .ok_or_else(|| MigrationError::Failed(format!("no {was:?}")))?;
        object.insert(now.into(), value);
        Ok(raw)
    }

    fn check(&self, code: &'static str) -> Vec<Warning> {
        if self.notes.is_empty() {
            vec![Warning::new(code, "no notes")]
        } else {
            Vec::new()
        }
    }

    /// Drops notes on removed Ids and the removed entries with them.
    fn reconcile(&mut self) {
        let old = std::mem::take(&mut self.refs);
        let notes = std::mem::take(&mut self.notes);
        for (at, text) in notes {
            let r = &old[at as usize];
            if !r.is_removed() {
                self.add(r.clone(), &text);
            }
        }
    }

    fn merge(&self, addon: &Self) -> Result<(Self, Receipt), MergeRefused> {
        let mut merged = self.clone();
        let mut added = Vec::new();
        for (at, text) in &addon.notes {
            if self.notes.iter().any(|(_, t)| t == text) {
                return Err(MergeRefused::Conflict(format!("note {text:?} exists")));
            }
            merged.add(addon.refs[*at as usize].clone(), text);
            added.push(text.clone());
        }
        Ok((merged, Receipt::new(added)))
    }

    fn unmerge(&mut self, receipt: &Receipt) {
        let added: &Vec<String> = receipt.get().unwrap();
        self.notes.retain(|(_, t)| !added.contains(t));
    }
}

macro_rules! notes_extension {
    ($name:ident, $key:literal) => {
        #[derive(Clone, Debug, Default, PartialEq)]
        struct $name(Notes);

        impl Extension for $name {
            const KEY: &'static str = $key;
            const VERSION: u32 = 2;

            fn decode(raw: RawExtension) -> Result<Self, DecodeError> {
                Notes::decode(raw).map(Self)
            }
            fn encode(&self) -> RawExtension {
                self.0.encode()
            }
            fn refs(&self) -> &[IdRef] {
                &self.0.refs
            }
            fn refs_mut(&mut self) -> &mut [IdRef] {
                &mut self.0.refs
            }
            fn migrate(from: u32, raw: RawExtension) -> Result<RawExtension, MigrationError> {
                Notes::migrate(from, raw)
            }
            fn check(&self, _model: &Model) -> Vec<Warning> {
                self.0.check(concat!($key, "/empty"))
            }
            fn reconcile(&mut self, _model: &Model) {
                self.0.reconcile()
            }
            fn merge_addon(&self, addon: &Self) -> Result<(Self, Receipt), MergeRefused> {
                self.0.merge(&addon.0).map(|(d, r)| (Self(d), r))
            }
            fn unmerge_addon(&mut self, receipt: &Receipt) {
                self.0.unmerge(receipt)
            }
        }
    };
}

notes_extension!(Newer, "test.newer");
notes_extension!(Migrated, "test.migrated");
notes_extension!(MigratedUnregistered, "test.migrated-unregistered");
notes_extension!(BadMigration, "test.bad-migration");
notes_extension!(BadDecode, "test.bad-decode");
notes_extension!(RenameTyped, "test.rename-typed");
notes_extension!(RemoveTyped, "test.remove-typed");
notes_extension!(Cached, "test.cached");
notes_extension!(CachedRemoval, "test.cached-removal");
notes_extension!(Shared, "test.shared");
notes_extension!(Checked, "test.checked");
notes_extension!(AddonMerge, "test.addon-merge");
notes_extension!(AddonAdded, "test.addon-added");
notes_extension!(AddonUnregistered, "test.addon-unregistered");
notes_extension!(Twice, "test.twice");
notes_extension!(TwiceOther, "test.twice");
notes_extension!(Reserved, "catchlight.reserved-by-test");
notes_extension!(ReservedOfficial, "catchlight.official-by-test");
impl sealed::Sealed for ReservedOfficial {}
impl Official for ReservedOfficial {}
notes_extension!(Plain, "test.plain");
notes_extension!(Removable, "test.removable");
notes_extension!(ReservedRemovable, "catchlight.removable-by-test");

fn key(text: &str) -> ExtensionKey {
    ExtensionKey::new(text).unwrap()
}

fn quad() -> ClmMesh {
    ClmMesh {
        verts: vec![-1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0],
        uvs: vec![0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0],
        indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
        origin: [0.0, 0.0],
    }
}

struct Rig {
    model: Model,
    part: NodeId,
    param: ParamId,
    texture: TexId,
    slot: SlotId,
}

impl Rig {
    fn refs(&self) -> Vec<IdRef> {
        vec![
            IdRef::Node(self.part.clone()),
            IdRef::Param(self.param.clone()),
            IdRef::Texture(self.texture.clone()),
            IdRef::Slot(self.part.clone(), self.slot.clone()),
        ]
    }
}

fn rig() -> Rig {
    let mut model = Model::new();
    let root = model.root().unwrap().clone();
    let part = NodeId::new("root/part-00000001").unwrap();
    model
        .add_node_with_id(
            part.clone(),
            &root,
            ModelNode::new("body", ModelNodeKind::Part(ModelPart::new(quad()))),
        )
        .unwrap();
    let param = ParamId::new("param-00000001").unwrap();
    model
        .add_param_with_id(
            param.clone(),
            ModelParam::new(Name::new("open").unwrap(), 0.0, 1.0, 0.0),
        )
        .unwrap();
    let texture = TexId::new("tex-00000001").unwrap();
    model
        .add_texture_with_id(
            texture.clone(),
            &part,
            ModelTexture {
                encoding: TextureEncoding::Png,
                alpha: TextureAlpha::Straight,
                data: vec![1, 2, 3, 4].into(),
            },
        )
        .unwrap();
    let slot = SlotId::new("slot-00000001").unwrap();
    model.slot_add(&part, slot.clone()).unwrap();
    Rig {
        model,
        part,
        param,
        texture,
        slot,
    }
}

/// A note on each of `refs`, as stored at `version` under the payload key
/// that version uses.
fn raw_notes(version: u32, refs: Vec<IdRef>) -> RawExtension {
    let field = ["old", "pairs", "notes"][version.min(2) as usize];
    let notes: Vec<(u32, String)> = (0..refs.len() as u32)
        .map(|i| (i, format!("note {i}")))
        .collect();
    RawExtension {
        version,
        refs,
        payload: Payload::Json(json!({ field: notes })),
    }
}

fn stored_raw(model: &Model, k: &str) -> RawExtension {
    match model.extensions().get(k) {
        Some(ExtensionValue::Maintained(raw)) => raw.clone(),
        other => panic!("{k}: not maintained: {other:?}"),
    }
}

fn warning_codes(model: &Model, k: &str) -> Vec<String> {
    model
        .extension_warnings()
        .into_iter()
        .filter(|w| w.key.as_str() == k)
        .map(|w| w.warning.code.to_string())
        .collect()
}

#[test]
fn an_unregistered_maintained_extension_survives_load_and_save_byte_for_byte() {
    let r = rig();
    let mut model = r.model.clone();
    model
        .set_extension(
            key("test.unregistered-json"),
            ExtensionValue::Maintained(raw_notes(7, r.refs())),
        )
        .unwrap();
    model
        .set_extension(
            key("test.unregistered-bytes"),
            ExtensionValue::Maintained(RawExtension {
                version: 3,
                refs: r.refs(),
                payload: Payload::Bytes(Arc::from(&b"\x00\xff\x10"[..])),
            }),
        )
        .unwrap();
    let first = model.to_clm_bytes().unwrap();
    let read = Model::from_clm_bytes(&first).unwrap();
    assert_eq!(read.extensions(), model.extensions());
    assert_eq!(read.to_clm_bytes().unwrap(), first);
    assert_eq!(
        stored_raw(&read, "test.unregistered-bytes").payload,
        Payload::Bytes(Arc::from(&b"\x00\xff\x10"[..]))
    );
}

#[test]
fn a_newer_version_is_carried_unchanged_and_warned_about() {
    register::<Newer>().unwrap();
    let r = rig();
    let mut model = r.model.clone();
    model
        .set_extension(
            key(Newer::KEY),
            ExtensionValue::Maintained(raw_notes(9, r.refs())),
        )
        .unwrap();
    let bytes = model.to_clm_bytes().unwrap();
    let read = Model::from_clm_bytes(&bytes).unwrap();
    assert_eq!(
        read.extension::<Newer>(),
        Err(ExtensionError::Unread(Unread::Newer {
            version: 9,
            readable: 2
        }))
    );
    assert_eq!(warning_codes(&read, Newer::KEY), [codes::NEWER]);
    assert!(
        read.check()
            .iter()
            .any(|w| w.message.contains("test.newer"))
    );
    assert_eq!(read.to_clm_bytes().unwrap(), bytes);
}

#[test]
fn an_older_version_migrates_step_by_step_and_saves_at_the_new_version() {
    register::<Migrated>().unwrap();
    let r = rig();
    let mut model = r.model.clone();
    model
        .set_extension(
            key(Migrated::KEY),
            ExtensionValue::Maintained(raw_notes(0, r.refs())),
        )
        .unwrap();
    let read = Model::from_clm_bytes(&model.to_clm_bytes().unwrap()).unwrap();
    assert_eq!(
        read.extension::<Migrated>().unwrap().0.texts(),
        ["note 0", "note 1", "note 2", "note 3"]
    );
    let saved = Model::from_clm_bytes(&read.to_clm_bytes().unwrap()).unwrap();
    assert_eq!(stored_raw(&saved, Migrated::KEY), raw_notes(2, r.refs()));

    // A typed read migrates without the registry, in memory only.
    model
        .set_extension(
            key(MigratedUnregistered::KEY),
            ExtensionValue::Maintained(raw_notes(1, r.refs())),
        )
        .unwrap();
    assert_eq!(
        model
            .extension::<MigratedUnregistered>()
            .unwrap()
            .0
            .notes
            .len(),
        4
    );
    assert_eq!(
        stored_raw(&model, MigratedUnregistered::KEY).version,
        1,
        "a raw slot is written back as stored"
    );
}

#[test]
fn a_failed_migration_or_decode_is_carried_unchanged_and_warned_about() {
    register::<BadMigration>().unwrap();
    register::<BadDecode>().unwrap();
    let r = rig();
    let mut model = r.model;
    let no_pairs = RawExtension {
        version: 1,
        refs: Vec::new(),
        payload: Payload::Json(json!({"wrong": []})),
    };
    let not_json = RawExtension {
        version: 2,
        refs: Vec::new(),
        payload: Payload::Bytes(Arc::from(&b"\x01"[..])),
    };
    model
        .set_extension(
            key(BadMigration::KEY),
            ExtensionValue::Maintained(no_pairs.clone()),
        )
        .unwrap();
    model
        .set_extension(
            key(BadDecode::KEY),
            ExtensionValue::Maintained(not_json.clone()),
        )
        .unwrap();
    let bytes = model.to_clm_bytes().unwrap();
    let read = Model::from_clm_bytes(&bytes).unwrap();

    assert!(matches!(
        read.extension::<BadMigration>(),
        Err(ExtensionError::Unread(Unread::Migration(_)))
    ));
    assert!(matches!(
        read.extension::<BadDecode>(),
        Err(ExtensionError::Unread(Unread::Decode(_)))
    ));
    assert_eq!(warning_codes(&read, BadMigration::KEY), [codes::UNREAD]);
    assert_eq!(warning_codes(&read, BadDecode::KEY), [codes::UNREAD]);
    assert_eq!(stored_raw(&read, BadMigration::KEY), no_pairs);
    assert_eq!(stored_raw(&read, BadDecode::KEY), not_json);
    assert_eq!(read.to_clm_bytes().unwrap(), bytes);
}

#[test]
fn renaming_each_kind_of_id_rewrites_typed_and_raw_tables() {
    let r = rig();
    let mut model = r.model.clone();
    let typed = model.extension_or_default::<RenameTyped>().unwrap();
    for (i, re) in r.refs().into_iter().enumerate() {
        typed.0.add(re, &format!("note {i}"));
    }
    model
        .set_extension(
            key("test.rename-raw"),
            ExtensionValue::Maintained(raw_notes(2, r.refs())),
        )
        .unwrap();

    let part = NodeId::new("root/part-0000000a").unwrap();
    let param = ParamId::new("param-0000000a").unwrap();
    let texture = TexId::new("tex-0000000a").unwrap();
    let slot = SlotId::new("slot-0000000a").unwrap();
    model.rename_node_id(&r.part, part.clone()).unwrap();
    model.rename_param_id(&r.param, param.clone()).unwrap();
    model.rename_tex_id(&r.texture, texture.clone()).unwrap();
    model.rename_slot(&part, &r.slot, slot.clone()).unwrap();

    let want = vec![
        IdRef::Node(part.clone()),
        IdRef::Param(param),
        IdRef::Texture(texture),
        IdRef::Slot(part, slot),
    ];
    assert_eq!(model.extension::<RenameTyped>().unwrap().0.refs, want);
    assert_eq!(stored_raw(&model, "test.rename-raw").refs, want);
    assert!(
        model
            .extension_warnings()
            .iter()
            .all(|w| w.warning.code != codes::DANGLING && w.warning.code != codes::REMOVED)
    );
}

#[test]
fn removing_an_id_marks_it_reconciles_typed_slots_and_leaves_raw_markers() {
    let r = rig();
    let mut model = r.model.clone();
    let typed = model.extension_or_default::<RemoveTyped>().unwrap();
    for (i, re) in r.refs().into_iter().enumerate() {
        typed.0.add(re, &format!("note {i}"));
    }
    model
        .set_extension(
            key("test.remove-raw"),
            ExtensionValue::Maintained(raw_notes(2, r.refs())),
        )
        .unwrap();

    model.delete_param(&r.param).unwrap();
    assert_eq!(
        model.extension::<RemoveTyped>().unwrap().0.texts(),
        ["note 0", "note 2", "note 3"],
        "reconcile dropped the note on the removed param"
    );
    let raw = stored_raw(&model, "test.remove-raw");
    assert_eq!(
        raw.refs[1],
        IdRef::Removed(RemovedRef::Param(r.param.clone()))
    );
    assert_eq!(
        warning_codes(&model, "test.remove-raw"),
        [codes::REMOVED, codes::NOT_CHECKED]
    );

    model.slot_delete(&r.part, &r.slot).unwrap();
    assert_eq!(
        stored_raw(&model, "test.remove-raw").refs[3],
        IdRef::Removed(RemovedRef::Slot(r.part.clone(), r.slot.clone()))
    );
    // Deleting the part also drops the texture only it drew.
    model.delete_node(&r.part).unwrap();
    let raw = stored_raw(&model, "test.remove-raw");
    assert!(raw.refs.iter().all(IdRef::is_removed), "{:?}", raw.refs);
    assert!(model.extension::<RemoveTyped>().unwrap().0.notes.is_empty());
    assert!(model.extension::<RemoveTyped>().unwrap().0.refs.is_empty());
}

#[test]
fn a_cached_typed_read_does_not_go_stale_after_an_id_change() {
    let r = rig();
    let mut model = r.model.clone();
    model
        .set_extension(
            key(Cached::KEY),
            ExtensionValue::Maintained(raw_notes(2, r.refs())),
        )
        .unwrap();
    assert_eq!(model.extension::<Cached>().unwrap().0.refs, r.refs());
    let param = ParamId::new("param-0000000b").unwrap();
    model.rename_param_id(&r.param, param.clone()).unwrap();
    assert_eq!(
        model.extension::<Cached>().unwrap().0.refs[1],
        IdRef::Param(param)
    );
    assert_eq!(
        model.extensions().refs(Cached::KEY).unwrap(),
        model.extension::<Cached>().unwrap().0.refs
    );

    // A removal on a cached slot reconciles the promoted data.
    model
        .set_extension(
            key(CachedRemoval::KEY),
            ExtensionValue::Maintained(raw_notes(2, r.refs())),
        )
        .unwrap();
    model.extension::<CachedRemoval>().unwrap();
    model.delete_texture(&r.texture).unwrap();
    assert_eq!(
        model.extension::<CachedRemoval>().unwrap().0.texts(),
        ["note 0", "note 1", "note 3"]
    );
}

#[test]
fn extension_mut_bumps_the_generation_and_leaves_a_snapshot_alone() {
    let r = rig();
    let mut model = r.model.clone();
    model
        .extension_or_default::<Shared>()
        .unwrap()
        .0
        .add(IdRef::Param(r.param.clone()), "first");
    let before = model.generation();
    model.extension_mut::<Shared>().unwrap();
    assert!(model.generation() > before);

    let snapshot = model.clone();
    model
        .extension_mut::<Shared>()
        .unwrap()
        .0
        .add(IdRef::Param(r.param), "second");
    assert_eq!(snapshot.extension::<Shared>().unwrap().0.texts(), ["first"]);
    assert_eq!(
        model.extension::<Shared>().unwrap().0.texts(),
        ["first", "second"]
    );
    assert!(!model.authored_eq(&snapshot).unwrap());
    let restored = snapshot.clone();
    assert!(restored.authored_eq(&snapshot).unwrap());
}

#[test]
fn remove_extension_drops_the_key_reserved_or_not_and_bumps_only_when_present() {
    let r = rig();
    let mut model = r.model;
    model.extension_or_default::<Removable>().unwrap();
    model.extension_or_default::<ReservedRemovable>().unwrap();
    let before = model.generation();
    assert!(model.remove_extension::<Removable>());
    assert!(model.remove_extension::<ReservedRemovable>());
    assert!(model.generation() > before);
    assert!(model.extensions().is_empty());

    let before = model.generation();
    assert!(!model.remove_extension::<Removable>());
    assert_eq!(model.generation(), before);
}

#[test]
fn check_reports_dangling_references_own_warnings_and_unregistered_keys() {
    register::<Checked>().unwrap();
    let r = rig();
    let mut model = r.model;
    let ghost = ParamId::new("param-ghost").unwrap();
    model
        .set_extension(
            key(Checked::KEY),
            ExtensionValue::Maintained(RawExtension {
                version: 2,
                refs: vec![IdRef::Param(ghost)],
                payload: Payload::Json(json!({"notes": []})),
            }),
        )
        .unwrap();
    model
        .set_extension(
            key("test.never-registered"),
            ExtensionValue::Maintained(raw_notes(2, Vec::new())),
        )
        .unwrap();
    assert_eq!(
        warning_codes(&model, Checked::KEY),
        [codes::DANGLING, "test.checked/empty"]
    );
    assert_eq!(
        warning_codes(&model, "test.never-registered"),
        [codes::NOT_CHECKED]
    );
    let messages: Vec<String> = model.check().into_iter().map(|w| w.message).collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("test.never-registered") && m.contains("not checked"))
    );
    assert!(messages.iter().any(|m| m.contains("param-ghost")));
}

/// The base without the rig's part, and an addon that is that part.
fn base_and_addon(r: &Rig) -> (Model, Model) {
    let addon = r.model.extract(std::slice::from_ref(&r.part));
    let mut base = r.model.clone();
    base.delete_node(&r.part).unwrap();
    (base, addon)
}

#[test]
fn install_merges_through_the_extension_and_uninstall_takes_back_its_receipt() {
    register::<AddonMerge>().unwrap();
    let r = rig();
    let (mut base, mut addon) = base_and_addon(&r);
    base.extension_or_default::<AddonMerge>()
        .unwrap()
        .0
        .add(IdRef::Param(r.param.clone()), "base");
    addon
        .extension_or_default::<AddonMerge>()
        .unwrap()
        .0
        .add(IdRef::Node(r.part.clone()), "hat");

    let installed = base.install(&addon).unwrap();
    assert_eq!(
        base.extension::<AddonMerge>().unwrap().0.texts(),
        ["base", "hat"]
    );
    assert_eq!(
        installed.extensions().collect::<Vec<_>>(),
        [&key(AddonMerge::KEY)]
    );
    base.uninstall(&installed);
    assert_eq!(base.extension::<AddonMerge>().unwrap().0.texts(), ["base"]);

    // A refusal leaves the base untouched.
    let mut clash = addon.clone();
    clash
        .extension_mut::<AddonMerge>()
        .unwrap()
        .0
        .add(IdRef::Node(r.part), "base");
    let before = base.clone();
    match base.install(&clash) {
        Err(InstallError::ExtensionRefused { key, why }) => {
            assert_eq!(key.as_str(), AddonMerge::KEY);
            assert!(matches!(why, MergeRefused::Conflict(_)));
        }
        other => panic!("{other:?}"),
    }
    assert!(base.authored_eq(&before).unwrap());
}

#[test]
fn install_merges_into_empty_data_when_the_base_lacks_it_and_uninstall_keeps_later_additions() {
    register::<AddonAdded>().unwrap();
    let r = rig();
    let (mut base, mut addon) = base_and_addon(&r);
    addon
        .extension_or_default::<AddonAdded>()
        .unwrap()
        .0
        .add(IdRef::Node(r.part.clone()), "hat");
    let installed = base.install(&addon).unwrap();
    assert_eq!(base.extension::<AddonAdded>().unwrap().0.texts(), ["hat"]);
    base.extension_mut::<AddonAdded>()
        .unwrap()
        .0
        .add(IdRef::Param(r.param), "scarf");
    base.uninstall(&installed);
    assert_eq!(base.extension::<AddonAdded>().unwrap().0.texts(), ["scarf"]);
}

#[test]
fn install_refuses_an_unregistered_extension_by_key() {
    let r = rig();
    let (mut base, mut addon) = base_and_addon(&r);
    addon
        .extension_or_default::<AddonUnregistered>()
        .unwrap()
        .0
        .add(IdRef::Node(r.part), "hat");
    // Through a file, so the addon holds raw data rather than a decoded copy.
    let addon = Model::from_clm_bytes_with(&addon.to_clm_bytes().unwrap(), ReadOptions::fragment())
        .unwrap();
    match base.install(&addon) {
        Err(InstallError::ExtensionNotRegistered { key }) => {
            assert_eq!(key.as_str(), AddonUnregistered::KEY);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_addons_requirements_include_what_its_reference_tables_name_except_textures() {
    let r = rig();
    let (_, mut addon) = base_and_addon(&r);
    let elsewhere = TexId::new("tex-elsewhere").unwrap();
    addon
        .set_extension(
            key("test.addon-requirements"),
            ExtensionValue::Maintained(raw_notes(
                2,
                vec![
                    IdRef::Node(r.part.clone()),
                    IdRef::Param(r.param.clone()),
                    IdRef::Texture(elsewhere.clone()),
                    IdRef::Texture(r.texture.clone()),
                ],
            )),
        )
        .unwrap();
    let from_extensions: Vec<Required> = addon
        .requirements()
        .iter()
        .filter(|req| req.field == "extension reference")
        .map(|req| req.id.clone())
        .collect();
    assert_eq!(from_extensions, [Required::Param(r.param)]);
    let dangling: Vec<Vec<IdRef>> = addon
        .extension_warnings()
        .into_iter()
        .filter(|w| w.warning.code == codes::DANGLING)
        .map(|w| w.warning.subjects)
        .collect();
    assert_eq!(dangling, [vec![IdRef::Texture(elsewhere)]]);
}

/// Records, on each reconcile, whether the model still held this extension.
#[derive(Clone, Debug, Default, PartialEq)]
struct Probe {
    refs: Vec<IdRef>,
    saw_itself: Vec<bool>,
}

impl Extension for Probe {
    const KEY: &'static str = "test.probe";
    const VERSION: u32 = 1;

    fn decode(raw: RawExtension) -> Result<Self, DecodeError> {
        Ok(Self {
            refs: raw.refs,
            saw_itself: Vec::new(),
        })
    }
    fn encode(&self) -> RawExtension {
        RawExtension {
            version: 1,
            refs: self.refs.clone(),
            payload: Payload::Json(json!(null)),
        }
    }
    fn refs(&self) -> &[IdRef] {
        &self.refs
    }
    fn refs_mut(&mut self) -> &mut [IdRef] {
        &mut self.refs
    }
    fn reconcile(&mut self, model: &Model) {
        self.saw_itself
            .push(model.extensions().contains_key(Self::KEY));
    }
}

#[test]
fn a_promoted_raw_slot_reconciles_out_of_the_model() {
    let r = rig();
    let mut model = r.model.clone();
    model
        .set_extension(
            key(Probe::KEY),
            ExtensionValue::Maintained(RawExtension {
                version: 1,
                refs: vec![IdRef::Param(r.param.clone())],
                payload: Payload::Json(json!(null)),
            }),
        )
        .unwrap();
    model.delete_param(&r.param).unwrap();
    assert_eq!(model.extension_mut::<Probe>().unwrap().saw_itself, [false]);
}

static COUNTED_DECODES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[derive(Clone, Debug, Default, PartialEq)]
struct Counted {
    refs: Vec<IdRef>,
    data: Vec<u8>,
}

impl Extension for Counted {
    const KEY: &'static str = "test.counted";
    const VERSION: u32 = 1;

    fn decode(raw: RawExtension) -> Result<Self, DecodeError> {
        COUNTED_DECODES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let data = raw
            .payload
            .bytes()
            .ok_or_else(|| DecodeError::new("not bytes"))?
            .to_vec();
        Ok(Self {
            refs: raw.refs,
            data,
        })
    }
    fn encode(&self) -> RawExtension {
        RawExtension {
            version: 1,
            refs: self.refs.clone(),
            payload: Payload::Bytes(self.data.as_slice().into()),
        }
    }
    fn refs(&self) -> &[IdRef] {
        &self.refs
    }
    fn refs_mut(&mut self) -> &mut [IdRef] {
        &mut self.refs
    }
}

#[test]
fn replacing_the_structure_decodes_only_extensions_that_changed() {
    register::<Counted>().unwrap();
    let r = rig();
    let mut server = r.model.clone();
    *server.extension_or_default::<Counted>().unwrap() = Counted {
        refs: vec![IdRef::Param(r.param.clone())],
        data: vec![7; 64],
    };
    let mut replica = Model::from_clm_bytes(&server.to_clm_bytes().unwrap()).unwrap();
    let bytes = |server: &Model| {
        server
            .extensions()
            .get(Counted::KEY)
            .and_then(|v| v.bytes().cloned())
    };
    let push = |replica: &mut Model, server: &Model| {
        let held = bytes(server);
        replica
            .replace_structure(
                &server.to_structure_bytes().unwrap(),
                |id| server.texture(id).cloned(),
                |_| held.clone(),
                None,
            )
            .unwrap();
    };
    let decodes = || COUNTED_DECODES.load(std::sync::atomic::Ordering::SeqCst);

    let before = decodes();
    server.set_param_default(&r.param, 0.5).unwrap();
    push(&mut replica, &server);
    assert_eq!(
        decodes(),
        before,
        "an unchanged extension is not decoded again"
    );

    server.extension_mut::<Counted>().unwrap().data[0] = 8;
    push(&mut replica, &server);
    assert_eq!(decodes(), before + 1);
    assert_eq!(replica.extension::<Counted>().unwrap().data[0], 8);
}

#[test]
fn register_is_idempotent_per_type_and_refuses_a_taken_or_reserved_key() {
    register::<Twice>().unwrap();
    register::<Twice>().unwrap();
    assert!(matches!(
        register::<TwiceOther>(),
        Err(RegisterError::Taken {
            key: "test.twice",
            ..
        })
    ));
    assert!(matches!(
        register::<Reserved>(),
        Err(RegisterError::Reserved {
            key: "catchlight.reserved-by-test"
        })
    ));
    assert!(matches!(
        register::<ReservedOfficial>(),
        Err(RegisterError::Reserved { .. })
    ));
    register_official::<ReservedOfficial>().unwrap();
}

#[test]
fn a_plain_extension_is_carried_exactly_and_never_maintained() {
    let r = rig();
    let mut model = r.model.clone();
    let value = json!({"param": r.param.as_str(), "refs": [{"param": r.param.as_str()}]});
    model
        .set_extension(key(Plain::KEY), ExtensionValue::Json(value.clone()))
        .unwrap();
    model
        .set_extension(
            key("test.plain-bytes"),
            ExtensionValue::Bytes(Arc::from(&b"param-00000001"[..])),
        )
        .unwrap();
    model
        .rename_param_id(&r.param, ParamId::new("param-0000000c").unwrap())
        .unwrap();
    assert_eq!(
        model.extensions().get(Plain::KEY),
        Some(&ExtensionValue::Json(value))
    );
    assert!(model.extensions().refs(Plain::KEY).is_none());
    assert_eq!(
        model.extension::<Plain>(),
        Err(ExtensionError::NotMaintained)
    );
    assert!(model.extension_warnings().is_empty());
    let bytes = model.to_clm_bytes().unwrap();
    let read = Model::from_clm_bytes(&bytes).unwrap();
    assert_eq!(read.extensions(), model.extensions());
    assert_eq!(read.to_clm_bytes().unwrap(), bytes);
}
