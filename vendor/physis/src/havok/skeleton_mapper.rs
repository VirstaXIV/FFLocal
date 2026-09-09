// SPDX-FileCopyrightText: 2026 FFLocal contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! `hkaSkeletonMapper`: how a pose of skeleton A (the base skeleton an animation was authored
//! for) maps onto skeleton B (the skeleton the file belongs to). Race skeletons other than the
//! Midlander-male base carry one so the game can play base-skeleton clips on them.

use crate::havok::object::HavokObject;
use crate::havok::skeleton::HavokSkeleton;
use crate::havok::transform::HavokTransform;
use core::cell::RefCell;
use std::sync::Arc;

#[derive(Debug)]
pub struct HavokSimpleMapping {
    pub bone_a: usize,
    pub bone_b: usize,
    /// Transform from B's bone into A's bone space (`M_B[b] = M_A[a] * a_from_b`).
    pub a_from_b: HavokTransform,
}

#[derive(Debug)]
pub struct HavokChainMapping {
    pub start_bone_a: usize,
    pub end_bone_a: usize,
    pub start_bone_b: usize,
    pub end_bone_b: usize,
    pub start_a_from_b: HavokTransform,
    pub end_a_from_b: HavokTransform,
}

#[derive(Debug)]
pub struct HavokSkeletonMapper {
    pub skeleton_a: HavokSkeleton,
    pub skeleton_b: HavokSkeleton,
    pub simple_mappings: Vec<HavokSimpleMapping>,
    pub chain_mappings: Vec<HavokChainMapping>,
    /// Bones of B with no mapping (keep their local pose).
    pub unmapped_bones: Vec<usize>,
    pub keep_unmapped_local: bool,
    /// 0 = ragdoll, 1 = retargeting.
    pub mapping_type: i32,
}

impl HavokSkeletonMapper {
    pub fn new(object: Arc<RefCell<HavokObject>>) -> Self {
        let root = object.borrow();
        let mapping = root.get("mapping").as_object();
        let mapping = mapping.borrow();
        let skeleton_a = HavokSkeleton::new(mapping.get("skeletonA").as_object());
        let skeleton_b = HavokSkeleton::new(mapping.get("skeletonB").as_object());
        // Tagfiles omit members at their default value, so a bone index of 0 is simply absent.
        let int = |o: &HavokObject, name: &str| o.try_get(name).map(|v| v.as_int() as usize).unwrap_or(0);
        let transform = |o: &HavokObject, name: &str| match o.try_get(name) {
            Some(v) => HavokTransform::new(v.as_vec()),
            None => HavokTransform::from_trs([0.0; 4], [0.0, 0.0, 0.0, 1.0], [1.0; 4]),
        };
        let empty = Vec::new();
        let simple_mappings = mapping
            .try_get("simpleMappings")
            .map(|v| v.as_array())
            .unwrap_or(&empty)
            .iter()
            .map(|x| {
                let m = x.as_object();
                let m = m.borrow();
                HavokSimpleMapping {
                    bone_a: int(&m, "boneA"),
                    bone_b: int(&m, "boneB"),
                    a_from_b: transform(&m, "aFromBTransform"),
                }
            })
            .collect();
        let chain_mappings = mapping
            .try_get("chainMappings")
            .map(|v| v.as_array())
            .unwrap_or(&empty)
            .iter()
            .map(|x| {
                let m = x.as_object();
                let m = m.borrow();
                HavokChainMapping {
                    start_bone_a: int(&m, "startBoneA"),
                    end_bone_a: int(&m, "endBoneA"),
                    start_bone_b: int(&m, "startBoneB"),
                    end_bone_b: int(&m, "endBoneB"),
                    start_a_from_b: transform(&m, "startAFromBTransform"),
                    end_a_from_b: transform(&m, "endAFromBTransform"),
                }
            })
            .collect();
        let unmapped_bones = mapping
            .try_get("unmappedBones")
            .map(|v| v.as_array().iter().map(|x| x.as_int() as usize).collect())
            .unwrap_or_default();
        let keep_unmapped_local = mapping.try_get("keepUnmappedLocal").map(|v| v.as_int() != 0).unwrap_or(true);
        let mapping_type = mapping.try_get("mappingType").map(|v| v.as_int() as i32).unwrap_or(1);
        Self {
            skeleton_a,
            skeleton_b,
            simple_mappings,
            chain_mappings,
            unmapped_bones,
            keep_unmapped_local,
            mapping_type,
        }
    }
}
