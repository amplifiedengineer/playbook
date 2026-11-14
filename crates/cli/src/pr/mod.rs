// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr`: the mechanical half of `/playbook:pr-create`.

pub mod body_rules;
pub mod checks;
pub mod comments;
pub mod create;
pub mod land;
pub mod merge;
pub mod prepare;
pub mod reply;
pub mod review;
pub mod rules;
pub mod shared;
pub mod stack;
pub mod stack_context;
pub mod stack_digest;
pub mod stack_map;
pub mod status;
pub mod text_rules;
pub mod wait;
