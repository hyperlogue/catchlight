//! `Debug` on a type that holds pixels, encoded bytes or per-vertex data
//! prints a summary whose length does not grow with the payload.

use std::fmt::Debug;
use std::sync::Arc;

use catchlight_core::extension::Payload;
use catchlight_core::formats::clm::{
    ClmBindingValues, ClmCell, ClmCells, ClmExtensionBlob, ClmIndices, ClmMesh, ClmTexture,
    ClmTextureBytes, ClmUnknownSection, TextureAlpha, TextureEncoding,
};
use catchlight_core::model::{ModelBindingValues, ModelMesh, ModelTexture};
use catchlight_core::{
    DecodedTexture, DeformStack, EncodedTexture, ExtensionKey, ExtensionValue, Mesh, MeshIndices,
    TexId, TextureFormat, Vec2,
};

const MEGABYTE: usize = 1 << 20;
const VERTICES: usize = 100_000;

fn assert_short(value: &impl Debug) {
    let printed = format!("{value:?}");
    assert!(
        printed.len() < 200,
        "{} characters: {}",
        printed.len(),
        &printed[..200]
    );
}

fn bytes() -> Arc<[u8]> {
    vec![0xAB; MEGABYTE].into()
}

fn clm_mesh() -> ClmMesh {
    ClmMesh {
        verts: vec![0.5; VERTICES * 2],
        uvs: vec![0.5; VERTICES * 2],
        indices: ClmIndices::U32((0..VERTICES as u32).collect()),
        origin: [0.0, 0.0],
    }
}

#[test]
fn a_model_texture_prints_its_byte_count() {
    let texture = ModelTexture {
        encoding: TextureEncoding::Png,
        alpha: TextureAlpha::Straight,
        data: bytes(),
    };
    assert_short(&texture);
    assert!(format!("{texture:?}").contains(&MEGABYTE.to_string()));
}

#[test]
fn an_encoded_texture_prints_its_byte_count() {
    assert_short(&EncodedTexture {
        format: TextureFormat::Png,
        data: bytes(),
        premultiplied: false,
    });
}

#[test]
fn a_decoded_texture_prints_its_size() {
    assert_short(&DecodedTexture {
        width: 512,
        height: 512,
        rgba: bytes(),
    });
}

#[test]
fn a_clm_texture_prints_its_byte_count() {
    let texture = ClmTexture {
        id: TexId::new("tex-0").unwrap(),
        encoding: TextureEncoding::Png,
        alpha: TextureAlpha::Straight,
        data: bytes().to_vec(),
    };
    assert_short(&texture);
    assert_short(&ClmTextureBytes::from(&texture));
}

#[test]
fn an_extension_blob_prints_its_byte_count() {
    assert_short(&ClmExtensionBlob {
        key: ExtensionKey::new("vendor.blob").unwrap(),
        data: bytes().to_vec(),
    });
}

#[test]
fn an_unknown_section_prints_its_byte_count() {
    assert_short(&ClmUnknownSection {
        kind: 99,
        data: bytes(),
    });
}

#[test]
fn a_byte_extension_value_prints_its_length() {
    assert_short(&ExtensionValue::Bytes(bytes()));
}

#[test]
fn a_byte_payload_prints_its_length() {
    assert_short(&Payload::Bytes(bytes()));
}

#[test]
fn a_mesh_prints_its_counts() {
    assert_short(&Mesh::new(
        vec![Vec2::ONE; VERTICES],
        vec![Vec2::ONE; VERTICES],
        MeshIndices::U32((0..VERTICES as u32).collect()),
        Vec2::ZERO,
    ));
}

#[test]
fn a_model_mesh_prints_its_counts() {
    assert_short(&ModelMesh::from(clm_mesh()));
}

#[test]
fn deform_binding_values_print_their_cell_count() {
    let cells = (0..4)
        .map(|x| ClmCell {
            x,
            y: 0,
            value: vec![0.25; VERTICES * 2],
        })
        .collect();
    assert_short(&ModelBindingValues::from(ClmBindingValues::Deform(
        ClmCells { cells },
    )));
}

#[test]
fn a_deform_stack_prints_its_counts() {
    assert_short(&DeformStack::new(VERTICES));
}
