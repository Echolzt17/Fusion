use super::{parse::valid_region, types::*};
use crate::Result;
use std::collections::{BTreeMap, BTreeSet};

pub fn validate_settings(settings: &NetworkSettings, nodes: &[StoredNode]) -> Result<()> {
    if settings.allowed_regions.len() > 64
        || settings.allowed_regions.iter().any(|r| !valid_region(r))
    {
        return Err("国家或地区筛选无效".into());
    }
    let regions: BTreeSet<_> = settings.allowed_regions.iter().collect();
    if regions.len() != settings.allowed_regions.len() {
        return Err("国家或地区不能重复".into());
    }
    if settings.region_overrides.len() > MAX_NODES
        || settings
            .region_overrides
            .iter()
            .any(|(id, region)| !nodes.iter().any(|n| n.id == *id) || !valid_region(region))
    {
        return Err("节点地区修正无效".into());
    }
    for ids in [&settings.node_order, &settings.excluded_node_ids] {
        if ids.len() > MAX_NODES
            || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
            || ids.iter().any(|id| !nodes.iter().any(|n| n.id == *id))
        {
            return Err("节点排序或排除列表无效".into());
        }
    }
    if settings
        .fixed_node_id
        .as_ref()
        .is_some_and(|id| !nodes.iter().any(|n| n.id == *id))
    {
        return Err("指定节点已不存在，请重新选择".into());
    }
    if settings.mode == SelectionMode::Fixed && settings.fixed_node_id.is_none() {
        return Err("固定模式需要选择一个节点".into());
    }
    if settings.enabled && nodes.is_empty() {
        return Err("请先导入可用订阅".into());
    }
    if settings.enabled && settings.allowed_regions.is_empty() {
        return Err("启用前请至少选择一个允许使用的国家或地区".into());
    }
    Ok(())
}

pub fn region<'a>(node: &'a StoredNode, settings: &'a NetworkSettings) -> &'a str {
    settings
        .region_overrides
        .get(&node.id)
        .map(String::as_str)
        .unwrap_or(&node.region)
}

pub fn select_node(
    nodes: &[StoredNode],
    settings: &NetworkSettings,
    health: &BTreeMap<String, NodeHealth>,
    current: Option<&str>,
    at: i64,
) -> Option<String> {
    let eligible = |node: &&StoredNode| {
        settings
            .allowed_regions
            .iter()
            .any(|r| r == region(node, settings))
            && (settings.mode == SelectionMode::Fixed || region(node, settings) != "UNKNOWN")
            && !settings.excluded_node_ids.contains(&node.id)
            && health.get(&node.id).is_none_or(|h| h.eligible(at))
    };
    if settings.mode == SelectionMode::Fixed {
        return nodes
            .iter()
            .find(|n| Some(n.id.as_str()) == settings.fixed_node_id.as_deref())
            .filter(eligible)
            .map(|n| n.id.clone());
    }
    // A recovered or slightly faster node never steals an existing healthy choice.
    if let Some(node) = nodes
        .iter()
        .find(|n| Some(n.id.as_str()) == current)
        .filter(eligible)
    {
        return Some(node.id.clone());
    }
    let current_region = nodes
        .iter()
        .find(|n| Some(n.id.as_str()) == current)
        .map(|n| region(n, settings));
    nodes
        .iter()
        .filter(eligible)
        .min_by_key(|node| {
            let rank = settings
                .node_order
                .iter()
                .position(|id| id == &node.id)
                .unwrap_or(usize::MAX);
            let same_region = current_region.is_some_and(|r| r == region(node, settings));
            let region_rank = settings
                .allowed_regions
                .iter()
                .position(|r| r == region(node, settings))
                .unwrap_or(usize::MAX);
            let source_order = nodes
                .iter()
                .position(|n| n.id == node.id)
                .unwrap_or(usize::MAX);
            (!same_region, region_rank, rank, source_order)
        })
        .map(|node| node.id.clone())
}
