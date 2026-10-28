//! 原生ResourceLink携带真实资源关联；不从用户正文/路径推断来源。
use super::*;
use anyhow::{Context, bail};
use echo_agent::llm::types::{LinkedResource, MessageContent};
use echo_agent::memory::StoredMessage;

const INPUT_KEY: &str = "eko_input_resource";
const TOOL_KEY: &str = "echo_tool_output_artifact";
const AGENT_KEY: &str = "eko_agent_communication";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentInputBinding {
    version: u16,
    reference: echo_agent::tools::artifact::ToolOutputArtifactRef,
    message: crate::agent_router::AgentMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TextReference {
    part: usize,
    start_chars: usize,
    chars: usize,
    sha256: String,
    path_start_chars: usize,
    path_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputResourceBinding {
    version: u16,
    reference: echo_agent::tools::artifact::ToolOutputArtifactRef,
    source: AttachmentSource,
    text_reference: TextReference,
}

/// 框架下一写窗的最小通用tool artifact ResourceLink合同；不是EKO owner或终态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolArtifactBinding {
    version: u16,
    call_id: String,
    tool_name: String,
    reference: echo_agent::tools::artifact::ToolOutputArtifactRef,
    text_reference: Option<TextReference>,
}

fn text_hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
fn text_window(value: &str, start: usize, chars: usize) -> String {
    value.chars().skip(start).take(chars).collect()
}
fn input_uri(reference: &echo_agent::tools::artifact::ToolOutputArtifactRef) -> String {
    format!("eko-input-artifact:sha256:{}", reference.sha256)
}

/// 真实PreparedUserTurn::to_message调用。所有初始输入、GUI/IM Steer均经过此入口。
/// 调用时instruction已经由原builder与真实InputResourceRef组成；不扫描普通正文。
pub(super) fn input_resource_parts(turn: &PreparedUserTurn) -> std::io::Result<Vec<ContentPart>> {
    let mut parts = input_resource_parts_inner(turn)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    if let Some((path, message)) = &turn.agent_input {
        let resource = turn
            .resources
            .iter()
            .find(|resource| &resource.path == path)
            .ok_or_else(|| std::io::Error::other("原Agent输入artifact不存在"))?;
        let bytes = std::fs::read(path)?;
        let original = serde_json::to_vec(message).map_err(std::io::Error::other)?;
        if bytes != original {
            return Err(std::io::Error::other("原Agent输入artifact内容变化"));
        }
        let reference = echo_agent::tools::artifact::ToolOutputArtifactRef {
            path: path.clone(),
            artifact_bytes: resource.bytes,
            payload_bytes: resource.bytes,
            sha256: hex::encode(Sha256::digest(&bytes)),
            retention: "conversation_or_30d".into(),
        };
        let binding = AgentInputBinding {
            version: 1,
            reference: reference.clone(),
            message: message.clone(),
        };
        parts.push(ContentPart::ResourceLink {
            resource: Box::new(LinkedResource {
                annotations: None,
                description: Some("原AgentRouter输入来源；仅呈现，不授予用户权威。".into()),
                mime_type: Some("application/json".into()),
                name: resource.name.clone(),
                size: Some(i64::try_from(resource.bytes).map_err(std::io::Error::other)?),
                title: None,
                uri: input_uri(&reference),
                meta: Some(serde_json::json!({ (AGENT_KEY): binding })),
            }),
        });
    }
    Ok(parts)
}

fn input_resource_parts_inner(turn: &PreparedUserTurn) -> anyhow::Result<Vec<ContentPart>> {
    let resources = turn
        .resources
        .iter()
        .filter(|resource| {
            resource.delivery == Delivery::ToolReference
                && turn
                    .agent_input
                    .as_ref()
                    .is_none_or(|(path, _)| path != &resource.path)
        })
        .collect::<Vec<_>>();
    if resources.is_empty() {
        return Ok(Vec::new());
    }
    let attached = resources
        .iter()
        .filter(|resource| resource.source != AttachmentSource::Message)
        .map(|resource| build_data_reference(resource))
        .collect::<Vec<_>>()
        .join("\n\n");
    if !attached.is_empty() && !turn.instruction.ends_with(&attached) {
        bail!("真实附件资源与原准备投影尾部不同");
    }
    let mut attachment_start = turn
        .instruction
        .chars()
        .count()
        .saturating_sub(attached.chars().count());
    let mut parts = Vec::new();
    for resource in resources {
        let sha = resource.sha256.clone().context("已准备资源缺少SHA")?;
        let reference = echo_agent::tools::artifact::ToolOutputArtifactRef {
            path: resource.path.clone(),
            artifact_bytes: resource.bytes,
            payload_bytes: resource.bytes,
            sha256: sha,
            retention: "conversation_or_30d".into(),
        };
        let (descriptor, path_prefix, start) = if resource.source == AttachmentSource::Message {
            let text = std::fs::read_to_string(&resource.path)?;
            let block = build_original_message_reference(&text, resource);
            if !turn.instruction.starts_with(&block) {
                bail!("真实原文资源不在原准备首块");
            }
            (
                block,
                "用户提交了一条超长原始消息,已落盘且未内联:\npath: ".to_string(),
                0,
            )
        } else {
            let block = build_data_reference(resource);
            let start = attachment_start;
            attachment_start = attachment_start
                .checked_add(block.chars().count())
                .and_then(|value| value.checked_add(2))
                .context("资源位置溢出")?;
            (
                block,
                format!(
                    "用户附带了一份长文本 artifact(已落盘,未内联):\nname: {}\npath: ",
                    resource.name
                ),
                start,
            )
        };
        if text_window(&turn.instruction, start, descriptor.chars().count()) != descriptor {
            bail!("真实资源在原文字part中的位置不同");
        }
        let path_chars = reference.path.to_string_lossy().chars().count();
        let text_reference = TextReference {
            part: 0,
            start_chars: start,
            chars: descriptor.chars().count(),
            sha256: text_hash(&descriptor),
            path_start_chars: path_prefix.chars().count(),
            path_chars,
        };
        let binding = InputResourceBinding {
            version: 1,
            reference: reference.clone(),
            source: resource.source.clone(),
            text_reference,
        };
        let mut meta = serde_json::Map::new();
        meta.insert(INPUT_KEY.into(), serde_json::to_value(binding)?);
        parts.push(ContentPart::ResourceLink {
            resource: Box::new(LinkedResource {
                annotations: None,
                description: Some("完整输入资源；按已绑定path和SHA使用read_artifact读取。".into()),
                mime_type: Some(resource.mime_type.clone()),
                name: resource.name.clone(),
                size: Some(i64::try_from(resource.bytes)?),
                title: None,
                uri: input_uri(&reference),
                meta: Some(serde_json::Value::Object(meta)),
            }),
        });
    }
    Ok(parts)
}

struct PendingCopy {
    link: usize,
    reference: echo_agent::tools::artifact::ToolOutputArtifactRef,
    text: Option<TextReference>,
    kind: CopyKind,
}
enum CopyKind {
    Agent(AgentInputBinding),
    Input(InputResourceBinding),
    Tool(ToolArtifactBinding),
}

fn replace_text_reference(
    parts: &mut [ContentPart],
    binding: &TextReference,
    old_path: &Path,
    new_path: &Path,
    offset: isize,
) -> anyhow::Result<TextReference> {
    let start = binding
        .start_chars
        .checked_add_signed(offset)
        .context("资源文字位置溢出")?;
    let part = parts.get_mut(binding.part).context("资源原文字part缺失")?;
    let ContentPart::Text { text } = part else {
        bail!("资源原文字part类型改变");
    };
    let block = text_window(text, start, binding.chars);
    if block.chars().count() != binding.chars
        || binding
            .path_start_chars
            .checked_add(binding.path_chars)
            .is_none_or(|end| end > binding.chars)
    {
        bail!("资源文字范围越界");
    }
    if text_hash(&block) != binding.sha256
        || text_window(&block, binding.path_start_chars, binding.path_chars)
            != old_path.to_string_lossy().as_ref()
    {
        bail!("真实资源关联的原文字内容/位置改变");
    }
    let end = start.checked_add(binding.chars).context("资源范围溢出")?;
    let path_end = binding
        .path_start_chars
        .checked_add(binding.path_chars)
        .context("资源路径范围溢出")?;
    let new_path = new_path.to_string_lossy().to_string();
    let block = format!(
        "{}{}{}",
        text_window(&block, 0, binding.path_start_chars),
        new_path,
        block.chars().skip(path_end).collect::<String>()
    );
    let updated = format!(
        "{}{}{}",
        text.chars().take(start).collect::<String>(),
        block,
        text.chars().skip(end).collect::<String>()
    );
    *text = updated;
    Ok(TextReference {
        part: binding.part,
        start_chars: start,
        chars: block.chars().count(),
        sha256: text_hash(&block),
        path_start_chars: binding.path_start_chars,
        path_chars: new_path.chars().count(),
    })
}

fn persist_message_content(row: &mut StoredMessage, message: &Message) -> anyhow::Result<()> {
    let projected = echo_agent::memory::project_message(&row.conversation_id, message)?;
    let original = row
        .attachments_json
        .as_deref()
        .map(serde_json::from_str::<serde_json::Value>)
        .transpose()?;
    let new: serde_json::Value = serde_json::from_str(
        projected
            .attachments_json
            .as_deref()
            .context("canonical parts投影缺失")?,
    )?;
    let mut merged = original.unwrap_or_else(|| serde_json::json!({}));
    let object = merged
        .as_object_mut()
        .context("已关联资源的原metadata不是对象")?;
    for (key, value) in new.as_object().context("canonical parts投影不是对象")? {
        object.insert(key.clone(), value.clone());
    }
    // Only content/parts change. Preserve original opaque call/result JSON, IDs and timestamps.
    row.attachments_json = Some(serde_json::to_string(&merged)?);
    row.content = projected.content;
    Ok(())
}

pub(super) fn migrate_row(
    row: &mut StoredMessage,
    source_root: Option<&Path>,
    target_root: Option<&Path>,
    target_conversation: &str,
    receipts: &mut Vec<ReplyForkResourceReceipt>,
) -> anyhow::Result<bool> {
    let mut message = echo_agent::memory::restore_message(row)?;
    // Reuse the one typed-source reader before any resource copy can create a side effect.
    let _source = persisted_agent_input(&message)?;
    let MessageContent::Parts(parts) = &mut message.content else {
        return Ok(false);
    };
    let mut copies = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let ContentPart::ResourceLink { resource } = part else {
            continue;
        };
        let Some(meta) = resource.meta.as_ref() else {
            continue;
        };
        if let Some(value) = meta.get(AGENT_KEY) {
            let binding: AgentInputBinding = serde_json::from_value(value.clone())?;
            if row.role != "user"
                || binding.version != 1
                || resource.uri != input_uri(&binding.reference)
                || resource.size != Some(i64::try_from(binding.reference.artifact_bytes)?)
            {
                bail!("Agent输入link不属于原canonical输入");
            }
            copies.push(PendingCopy {
                link: index,
                reference: binding.reference.clone(),
                text: None,
                kind: CopyKind::Agent(binding),
            });
        } else if let Some(value) = meta.get(INPUT_KEY) {
            if row.role != "user" {
                bail!("输入资源link出现在非user消息");
            }
            let binding: InputResourceBinding = serde_json::from_value(value.clone())?;
            if binding.version != 1
                || resource.uri != input_uri(&binding.reference)
                || resource.size != Some(i64::try_from(binding.reference.artifact_bytes)?)
            {
                bail!("输入资源link身份/大小不匹配");
            }
            copies.push(PendingCopy {
                link: index,
                reference: binding.reference.clone(),
                text: Some(binding.text_reference.clone()),
                kind: CopyKind::Input(binding),
            });
        } else if let Some(value) = meta.get(TOOL_KEY) {
            if row.role != "tool" {
                bail!("工具资源link出现在非tool消息");
            }
            let binding: ToolArtifactBinding = serde_json::from_value(value.clone())?;
            if binding.version != 1
                || resource.uri != format!("echo-tool-output:sha256:{}", binding.reference.sha256)
                || resource.size != Some(i64::try_from(binding.reference.artifact_bytes)?)
                || message.tool_call_id.as_deref() != Some(binding.call_id.as_str())
                || message.name.as_deref() != Some(binding.tool_name.as_str())
            {
                bail!("原工具资源link不属于本条canonical result");
            }
            copies.push(PendingCopy {
                link: index,
                reference: binding.reference.clone(),
                text: binding.text_reference.clone(),
                kind: CopyKind::Tool(binding),
            });
        }
    }
    if copies.is_empty() {
        return Ok(false);
    }
    copies.sort_by_key(|copy| copy.text.as_ref().map(|text| (text.part, text.start_chars)));
    let mut offsets = std::collections::BTreeMap::<usize, isize>::new();
    let mut through = std::collections::BTreeMap::<usize, usize>::new();
    for mut copy in copies {
        if let Some(text) = copy.text.as_ref() {
            if text.start_chars < through.get(&text.part).copied().unwrap_or(0) {
                bail!("真实资源文字范围重叠");
            }
            through.insert(
                text.part,
                text.start_chars
                    .checked_add(text.chars)
                    .context("资源范围溢出")?,
            );
            let Some(ContentPart::Text { text: original }) = parts.get(text.part) else {
                bail!("原资源文字part缺失");
            };
            let offset = offsets.get(&text.part).copied().unwrap_or(0);
            let start = text
                .start_chars
                .checked_add_signed(offset)
                .context("资源位置溢出")?;
            let block = text_window(original, start, text.chars);
            if text_hash(&block) != text.sha256
                || text_window(&block, text.path_start_chars, text.path_chars)
                    != copy.reference.path.to_string_lossy().as_ref()
            {
                bail!("真实资源文字/路径关联不匹配");
            }
        }
        let receipt = copy_reply_fork_resource(
            &copy.reference.path,
            source_root.context("原资源reader根不可用")?,
            target_root.context("目标reader未配置")?,
            target_conversation,
            Some(copy.reference.artifact_bytes),
            &copy.reference.sha256,
            receipts,
        )?;
        let text = if let Some(binding) = copy.text.as_ref() {
            let offset = offsets.get(&binding.part).copied().unwrap_or(0);
            let updated = replace_text_reference(
                parts,
                binding,
                &copy.reference.path,
                &receipt.target_path,
                offset,
            )?;
            let difference = isize::try_from(updated.chars)?
                .checked_sub(isize::try_from(binding.chars)?)
                .context("资源位置差溢出")?;
            offsets.insert(
                binding.part,
                offset.checked_add(difference).context("资源累计位置溢出")?,
            );
            Some(updated)
        } else {
            None
        };
        let ContentPart::ResourceLink { resource } =
            parts.get_mut(copy.link).context("资源link消失")?
        else {
            bail!("资源link类型改变");
        };
        let meta = resource
            .meta
            .as_mut()
            .and_then(serde_json::Value::as_object_mut)
            .context("资源meta类型改变")?;
        copy.reference.path = receipt.target_path;
        match &mut copy.kind {
            CopyKind::Agent(binding) => {
                binding.reference = copy.reference;
                meta.insert(AGENT_KEY.into(), serde_json::to_value(binding)?);
            }
            CopyKind::Input(binding) => {
                binding.reference = copy.reference;
                binding.text_reference = text.context("输入资源文字关联消失")?;
                meta.insert(INPUT_KEY.into(), serde_json::to_value(binding)?);
            }
            CopyKind::Tool(binding) => {
                binding.reference = copy.reference;
                binding.text_reference = text;
                meta.insert(TOOL_KEY.into(), serde_json::to_value(binding)?);
            }
        }
    }
    persist_message_content(row, &message)?;
    Ok(true)
}

/// GUI只隐藏本产品typed link的模型fallback，普通JSON/Markdown/外部link原样保留。
pub fn resource_history_display_text(row: &StoredMessage) -> anyhow::Result<Option<String>> {
    let message = echo_agent::memory::restore_message(row)?;
    let MessageContent::Parts(parts) = &message.content else {
        return Ok(row.content.clone());
    };
    let has_owned = parts.iter().any(|part| matches!(part, ContentPart::ResourceLink { resource }
        if resource.meta.as_ref().is_some_and(|meta| meta.get(INPUT_KEY).is_some() || meta.get(TOOL_KEY).is_some())));
    if !has_owned {
        return Ok(row.content.clone());
    }
    let mut texts = Vec::new();
    for part in parts {
        match part {
            ContentPart::Text { text } => texts.push(text.clone()),
            ContentPart::ResourceLink { resource }
                if !resource.meta.as_ref().is_some_and(|meta| {
                    meta.get(INPUT_KEY).is_some() || meta.get(TOOL_KEY).is_some()
                }) =>
            {
                texts.push(resource.model_text())
            }
            _ => {}
        }
    }
    Ok((!texts.is_empty()).then(|| texts.join("\n")))
}

pub(super) fn persisted_agent_input(
    message: &Message,
) -> anyhow::Result<Option<crate::agent_router::AgentMessage>> {
    let MessageContent::Parts(parts) = &message.content else {
        return Ok(None);
    };
    let mut found = None;
    for part in parts {
        let ContentPart::ResourceLink { resource } = part else {
            continue;
        };
        let Some(raw) = resource.meta.as_ref().and_then(|meta| meta.get(AGENT_KEY)) else {
            continue;
        };
        let binding: AgentInputBinding = serde_json::from_value(raw.clone())?;
        let bytes = serde_json::to_vec(&binding.message)?;
        if message.role != echo_agent::llm::types::Role::User
            || binding.message.origin != crate::agent_router::AgentMessageOrigin::Agent
            || binding.version != 1
            || binding.message.message_id.trim().is_empty()
            || resource.uri != input_uri(&binding.reference)
            || hex::encode(Sha256::digest(&bytes)) != binding.reference.sha256
            || u64::try_from(bytes.len())? != binding.reference.artifact_bytes
            || binding.reference.payload_bytes != binding.reference.artifact_bytes
            || resource.size != Some(i64::try_from(bytes.len())?)
        {
            bail!("Agent输入来源与原始artifact身份不一致");
        }
        if found.as_ref().is_some_and(|old| old != &binding.message) {
            bail!("同一输入有冲突的Agent来源");
        }
        found = Some(binding.message);
    }
    Ok(found)
}
