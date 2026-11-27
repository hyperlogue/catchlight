//! Textures, kept only while a part draws them.

use super::*;

/// Source-encoded texture bytes (PNG/TGA), immutable under its Id. `Arc<[u8]>`
/// matches [`crate::texture::EncodedTexture`], so decoding shares the allocation.
#[derive(Clone)]
pub struct ModelTexture {
    pub encoding: TextureEncoding,
    pub alpha: TextureAlpha,
    pub data: Arc<[u8]>,
}

impl std::fmt::Debug for ModelTexture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelTexture")
            .field("encoding", &self.encoding)
            .field("alpha", &self.alpha)
            .field("bytes", &self.data.len())
            .finish_non_exhaustive()
    }
}

impl From<&ModelTexture> for crate::texture::EncodedTexture {
    fn from(tex: &ModelTexture) -> Self {
        Self {
            format: match tex.encoding {
                TextureEncoding::Png => crate::texture::TextureFormat::Png,
                TextureEncoding::Tga => crate::texture::TextureFormat::Tga,
            },
            data: Arc::clone(&tex.data),
            premultiplied: tex.alpha == TextureAlpha::PremultipliedSrgb,
        }
    }
}

impl Model {
    /// Deletes the texture left behind if this part was its last user.
    pub fn set_part_albedo(
        &mut self,
        id: &NodeId,
        albedo: Option<TexId>,
    ) -> Result<(), ModelError> {
        if let Some(albedo) = &albedo
            && !self.textures.contains_key(albedo)
        {
            return Err(ModelError::UnknownTexture(albedo.clone()));
        }
        match self.nodes.get(id).map(|n| &n.kind) {
            Some(ModelNodeKind::Part(_)) => {}
            Some(_) => return Err(ModelError::NotAPart),
            None => return Err(ModelError::UnknownNode(id.clone())),
        }
        self.point_albedo(id, albedo);
        self.bump();
        Ok(())
    }

    pub fn texture_ids(&self) -> &[TexId] {
        &self.texture_order
    }

    pub fn texture(&self, id: &TexId) -> Option<&ModelTexture> {
        self.textures.get(id)
    }

    /// Displaces `part`'s old albedo as [`Self::set_part_albedo`] does.
    pub fn add_texture(
        &mut self,
        part: &NodeId,
        texture: ModelTexture,
        hex: &mut impl HexSource,
    ) -> Result<TexId, ModelError> {
        let id = self.mint(TexId::generate, |m, id| m.textures.contains_key(id), hex)?;
        self.add_texture_with_id(id.clone(), part, texture)?;
        Ok(id)
    }

    pub fn add_texture_with_id(
        &mut self,
        id: TexId,
        part: &NodeId,
        texture: ModelTexture,
    ) -> Result<(), ModelError> {
        if self.textures.contains_key(&id) {
            return Err(ModelError::DuplicateId(id.into()));
        }
        match self.nodes.get(part).map(|n| &n.kind) {
            Some(ModelNodeKind::Part(_)) => {}
            Some(_) => return Err(ModelError::NotAPart),
            None => return Err(ModelError::UnknownNode(part.clone())),
        }
        self.textures.insert(id.clone(), texture);
        self.texture_order.push(id.clone());
        self.point_albedo(part, Some(id));
        self.bump();
        Ok(())
    }

    pub fn delete_texture(&mut self, id: &TexId) -> Result<(), ModelError> {
        if self.textures.remove(id).is_none() {
            return Err(ModelError::UnknownTexture(id.clone()));
        }
        self.texture_order.retain(|t| t != id);
        for node in self.nodes.values_mut() {
            if let ModelNodeKind::Part(p) = &mut node.kind
                && p.albedo.as_ref() == Some(id)
            {
                p.albedo = None;
            }
        }
        self.remove_in_extensions(|r| matches!(r, IdRef::Texture(t) if t == id));
        self.bump();
        Ok(())
    }

    /// The textures [`Self::delete_node`] would drop, in model texture order.
    pub fn textures_dropped_by_deleting(&self, id: &NodeId) -> Vec<TexId> {
        let removed: HashSet<NodeId> = self.subtree(id).into_iter().collect();
        let kept: HashSet<&TexId> = self
            .nodes
            .iter()
            .filter(|(id, _)| !removed.contains(*id))
            .filter_map(|(_, n)| albedo_of(n))
            .collect();
        let going: HashSet<&TexId> = removed
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .filter_map(albedo_of)
            .collect();
        self.texture_order
            .iter()
            .filter(|t| going.contains(t) && !kept.contains(t))
            .cloned()
            .collect()
    }

    /// The texture [`Self::set_part_albedo`] or [`Self::add_texture`] would drop.
    pub fn texture_dropped_by_repointing(
        &self,
        part: &NodeId,
        albedo: Option<&TexId>,
    ) -> Option<TexId> {
        let old = self.nodes.get(part).and_then(albedo_of)?;
        if Some(old) == albedo {
            return None;
        }
        self.nodes
            .iter()
            .all(|(id, n)| id == part || albedo_of(n) != Some(old))
            .then(|| old.clone())
    }

    /// The caller checks both Ids and bumps.
    fn point_albedo(&mut self, part: &NodeId, albedo: Option<TexId>) {
        let old = match self.nodes.get_mut(part).map(|n| &mut n.kind) {
            Some(ModelNodeKind::Part(p)) => std::mem::replace(&mut p.albedo, albedo),
            _ => return,
        };
        if let Some(old) = old
            && !self.nodes.values().any(|n| albedo_of(n) == Some(&old))
        {
            self.textures.remove(&old);
            self.texture_order.retain(|t| t != &old);
            self.remove_in_extensions(|r| matches!(r, IdRef::Texture(t) if t == &old));
        }
    }

    pub(super) fn gc_textures(&mut self) {
        let drawn: HashSet<TexId> = self.nodes.values().filter_map(albedo_of).cloned().collect();
        let dropped: HashSet<TexId> = self
            .texture_order
            .iter()
            .filter(|id| !drawn.contains(*id))
            .cloned()
            .collect();
        self.textures.retain(|id, _| drawn.contains(id));
        self.texture_order.retain(|id| drawn.contains(id));
        if !dropped.is_empty() {
            self.remove_in_extensions(|r| matches!(r, IdRef::Texture(t) if dropped.contains(t)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::*;

    #[test]
    fn adding_a_texture_points_a_part_at_it_in_one_edit() {
        let mut r = fixture();
        let (part, before) = (r.part.clone(), r.model.generation());

        let tex = r.model.add_texture(&part, texture(), &mut r.hex).unwrap();

        assert_eq!(albedo(&r.model, &part), Some(tex.clone()));
        assert!(r.model.texture(&tex).is_some());
        assert!(
            r.model.generation() > before,
            "one edit, and it moved the clock"
        );
    }

    #[test]
    fn a_texture_needs_a_part_to_draw_it() {
        let mut r = fixture();
        let (composite, stranger) = (r.composite.clone(), NodeId::new("gone").unwrap());
        let before = r.model.generation();

        assert!(matches!(
            r.model.add_texture(&composite, texture(), &mut r.hex),
            Err(ModelError::NotAPart)
        ));
        assert!(matches!(
            r.model.add_texture(&stranger, texture(), &mut r.hex),
            Err(ModelError::UnknownNode(id)) if id == stranger
        ));
        assert!(matches!(
            r.model
                .add_texture_with_id(TexId::new("chosen").unwrap(), &composite, texture()),
            Err(ModelError::NotAPart)
        ));
        assert_eq!(r.model.texture_ids().len(), 1, "nothing landed");
        assert_eq!(r.model.generation(), before, "and nothing moved the clock");
    }

    #[test]
    fn repointing_the_last_part_drawing_a_texture_deletes_it() {
        for albedo_after in [None, Some(())] {
            let mut r = fixture();
            let (part, other, old) = (r.part.clone(), r.other.clone(), r.tex.clone());
            // `other` draws `old`; hand it to `part` as well, then take it
            // back off `other` — two users, so nothing goes.
            r.model.set_part_albedo(&part, Some(old.clone())).unwrap();
            r.model.set_part_albedo(&other, None).unwrap();
            assert!(
                r.model.texture(&old).is_some(),
                "a texture a part still draws stays"
            );

            let next = albedo_after.map(|()| {
                r.model
                    .add_texture(&other, texture(), &mut r.hex)
                    .expect("a second texture")
            });
            assert_eq!(
                r.model.texture_dropped_by_repointing(&part, next.as_ref()),
                Some(old.clone()),
                "the query and the edit have to agree"
            );
            let before = r.model.generation();
            r.model.set_part_albedo(&part, next.clone()).unwrap();

            assert!(r.model.texture(&old).is_none(), "the last user let go");
            assert!(
                !r.model.texture_ids().contains(&old),
                "and the order with it"
            );
            assert_eq!(albedo(&r.model, &part), next);
            assert!(r.model.generation() > before, "one edit, one bump");
        }
    }

    #[test]
    fn an_upload_takes_the_texture_it_displaces() {
        let mut r = fixture();
        let (other, old) = (r.other.clone(), r.tex.clone());

        assert_eq!(
            r.model.texture_dropped_by_repointing(&other, None),
            Some(old)
        );
        let new = r.model.add_texture(&other, texture(), &mut r.hex).unwrap();

        assert_eq!(r.model.texture_ids(), std::slice::from_ref(&new));
        assert_eq!(albedo(&r.model, &other), Some(new));
    }

    #[test]
    fn a_snapshot_taken_before_the_cascade_still_has_the_texture() {
        let mut r = fixture();
        let (other, old) = (r.other.clone(), r.tex.clone());
        let snapshot = r.model.clone();

        r.model.set_part_albedo(&other, None).unwrap();
        assert!(r.model.texture(&old).is_none());

        assert!(snapshot.texture(&old).is_some());
        assert_eq!(snapshot.texture_ids(), std::slice::from_ref(&old));
    }

    #[test]
    fn deleting_a_subtree_takes_only_the_textures_nothing_else_draws() {
        let mut r = fixture();
        let (part, other) = (r.part.clone(), r.other.clone());
        let shared = r.tex.clone();
        // `other` draws `shared` and stays behind. Under `part`: a child
        // drawing `shared` too, and one with a texture of its own.
        let sharer = r
            .model
            .add_node(&part, ModelNode::new("sharer", part_kind()), &mut r.hex)
            .unwrap();
        r.model
            .set_part_albedo(&sharer, Some(shared.clone()))
            .unwrap();
        let owner = r
            .model
            .add_node(&part, ModelNode::new("owner", part_kind()), &mut r.hex)
            .unwrap();
        let exclusive = r.model.add_texture(&owner, texture(), &mut r.hex).unwrap();

        assert_eq!(
            r.model.textures_dropped_by_deleting(&part),
            vec![exclusive.clone()],
            "the query and the edit have to agree"
        );
        r.model.delete_node(&part).unwrap();

        assert!(
            r.model.texture(&exclusive).is_none(),
            "nothing draws it now"
        );
        assert!(
            r.model.texture(&shared).is_some(),
            "a part outside the subtree still draws it"
        );
        assert_eq!(r.model.texture_ids(), std::slice::from_ref(&shared));
        assert_eq!(albedo(&r.model, &other), Some(shared));
    }

    #[test]
    fn no_edit_leaves_a_texture_with_no_part_drawing_it() {
        #[expect(
            clippy::type_complexity,
            reason = "a table of named edits reads clearer inline than behind an alias"
        )]
        let edits: Vec<(&str, Box<dyn Fn(&mut Fixture)>)> = vec![
            (
                "set_part_albedo(None)",
                Box::new(|r| r.model.set_part_albedo(&r.other, None).unwrap()),
            ),
            (
                "add_texture over the old one",
                Box::new(|r| {
                    let t = texture();
                    r.model.add_texture(&r.other, t, &mut r.hex).unwrap();
                }),
            ),
            (
                "delete_node",
                Box::new(|r| r.model.delete_node(&r.other).unwrap()),
            ),
            (
                "delete_texture",
                Box::new(|r| r.model.delete_texture(&r.tex).unwrap()),
            ),
        ];
        for (name, edit) in edits {
            let mut r = fixture();
            edit(&mut r);
            let drawn: HashSet<&TexId> = r.model.nodes.values().filter_map(albedo_of).collect();
            for id in r.model.texture_ids() {
                assert!(
                    drawn.contains(id),
                    "{name} left {id} with no part drawing it"
                );
            }
        }
    }
}
