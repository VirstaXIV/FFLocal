// SPDX-FileCopyrightText: 2020 Inseok Lee
// SPDX-License-Identifier: MIT

use crate::havok::object::HavokObject;
use crate::havok::transform::HavokTransform;
use core::cell::RefCell;
use std::sync::Arc;

#[derive(Debug)]
pub struct HavokSkeleton {
    /// Skeleton name (e.g. `c0101`), when present.
    pub name: String,
    pub bone_names: Vec<String>,
    pub parent_indices: Vec<usize>,
    pub reference_pose: Vec<HavokTransform>,
}

impl HavokSkeleton {
    pub fn new(object: Arc<RefCell<HavokObject>>) -> Self {
        let root = object.borrow();
        let bones = root.get("bones").as_array();
        let bone_names = bones
            .iter()
            .map(|x| {
                let bone = x.as_object();
                let bone_obj = bone.borrow();

                bone_obj.get("name").as_string().to_owned()
            })
            .collect::<Vec<_>>();

        let raw_parent_indices = root.get("parentIndices").as_array();
        let parent_indices = raw_parent_indices
            .iter()
            .map(|x| x.as_int() as usize)
            .collect::<Vec<_>>();

        let raw_reference_pose = root.get("referencePose").as_array();
        let reference_pose = raw_reference_pose
            .iter()
            .map(|x| HavokTransform::new(x.as_vec()))
            .collect::<Vec<_>>();

        let name = root.try_get("name").map(|v| v.as_string().to_owned()).unwrap_or_default();

        Self {
            name,
            bone_names,
            parent_indices,
            reference_pose,
        }
    }
}
