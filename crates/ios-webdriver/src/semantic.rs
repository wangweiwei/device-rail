use std::collections::HashMap;

use devicerail_core::{DriverError, DriverResult, ExecutionControl};
use devicerail_protocol::{
    ElementSelector, ElementTarget, MAX_UI_IDENTIFIER_LENGTH, MAX_UI_ROLE_LENGTH,
    MAX_UI_SNAPSHOT_NODES, MAX_UI_TEXT_LENGTH, TextMatchMode, UI_SNAPSHOT_FORMAT_VERSION,
    UiContextKind, UiContextRef, UiNode, UiNodeRef, UiRect, UiSnapshot, Viewport,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    AppiumContext, AppiumElement, AppiumLocatorStrategy, AppiumSession, AppiumTransport,
    appium::MAX_LOCATOR_CHARS,
};

const MAX_NATIVE_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const MAX_WEB_RESULT_BYTES: usize = 8 * 1024 * 1024;
const MAX_NATIVE_CONTENT_TYPE_LENGTH: usize = 256;
const MAX_WEB_PATH_LENGTH: usize = MAX_UI_IDENTIFIER_LENGTH - "web:".len();
const MAX_WEB_XPATH_LENGTH: usize = MAX_LOCATOR_CHARS;

const WEB_SNAPSHOT_SCRIPT: &str = r#"
return (() => {
  const MAX_NODES = 10000;
  const MAX_TEXT = 4096;
  const MAX_NAME_REFERENCES = 128;
  const MAX_PATH = 4092;
  const MAX_XPATH = 16384;
  const nodes = [];
  let truncated = false;
  const documentToken = (() => {
    const key = '__devicerailDocumentTokenV1__';
    if (!Object.prototype.hasOwnProperty.call(document, key)) {
      const words = new Uint32Array(4);
      if (globalThis.crypto && typeof globalThis.crypto.getRandomValues === 'function') {
        globalThis.crypto.getRandomValues(words);
      } else {
        for (let index = 0; index < words.length; index += 1) {
          words[index] = Math.floor(Math.random() * 0x100000000);
        }
      }
      Object.defineProperty(document, key, {
        configurable: false,
        enumerable: false,
        value: Array.from(words, (word) => word.toString(16).padStart(8, '0')).join(''),
        writable: false
      });
    }
    return document[key];
  })();
  const lowerAttribute = (el, name, fallback = '', maxLength = MAX_TEXT) =>
    (el.getAttribute(name) || fallback).slice(0, maxLength).toLowerCase();
  const implicitRole = (el) => {
    const explicit = el.getAttribute('role');
    if (explicit) return explicit.slice(0, MAX_TEXT).trim().toLowerCase().split(/\s+/)[0];
    const tag = el.localName.toLowerCase();
    if (tag === 'button') return 'button';
    if (tag === 'a' && el.hasAttribute('href')) return 'link';
    if (tag === 'img') return 'image';
    if (tag === 'select') return 'combobox';
    if (tag === 'textarea') return 'textbox';
    if (tag === 'input') {
      const type = lowerAttribute(el, 'type', 'text', 64);
      if (['button', 'submit', 'reset', 'image'].includes(type)) return 'button';
      if (type === 'checkbox') return 'checkbox';
      if (type === 'radio') return 'radio';
      if (type === 'range') return 'slider';
      if (type === 'number') return 'spinbutton';
      if (type === 'search') return 'searchbox';
      return 'textbox';
    }
    if (/^h[1-6]$/.test(tag)) return 'heading';
    if (tag === 'ul' || tag === 'ol') return 'list';
    if (tag === 'li') return 'listitem';
    if (tag === 'table') return 'table';
    if (tag === 'tr') return 'row';
    if (tag === 'td' || tag === 'th') return 'cell';
    if (tag === 'form') return 'form';
    if (tag === 'nav') return 'navigation';
    if (tag === 'main') return 'main';
    return tag || 'generic';
  };
  const SKIPPED_TAGS = ['script', 'style', 'noscript', 'template'];
  const text = (value) => {
    if (value === null || value === undefined) return null;
    const normalized = String(value).slice(0, MAX_TEXT).trim();
    return normalized.length ? normalized : null;
  };
  const appendText = (current, value) => {
    if (current && current.length >= MAX_TEXT) return current;
    const piece = text(value);
    if (!piece) return current;
    if (!current) return piece;
    return `${current} ${piece}`.slice(0, MAX_TEXT);
  };
  const sensitive = (el) => {
    const type = lowerAttribute(el, 'type', '', 64);
    const autocomplete = (el.getAttribute('autocomplete') || '')
      .slice(0, MAX_TEXT).trim().toLowerCase().split(/[\t\n\f\r ]+/).filter(Boolean);
    return type === 'password' || autocomplete.some((token) =>
      ['current-password', 'new-password', 'one-time-code'].includes(token));
  };
  const sensitiveState = new WeakMap();
  const descendantTextCache = new WeakMap();
  const xpathPartCache = new WeakMap();
  const textEntries = [];
  if (document.documentElement) {
    xpathPartCache.set(document.documentElement,
      `${document.documentElement.localName.toLowerCase()}[1]`);
    const pending = [{el: document.documentElement, inheritedSensitive: false}];
    while (pending.length) {
      const {el, inheritedSensitive} = pending.pop();
      const tag = el.localName.toLowerCase();
      if (SKIPPED_TAGS.includes(tag)) continue;
      if (textEntries.length >= MAX_NODES) {
        truncated = true;
        break;
      }
      const isSensitive = inheritedSensitive || sensitive(el);
      sensitiveState.set(el, isSensitive);
      textEntries.push(el);
      const children = Array.from(el.children);
      const tagIndexes = new Map();
      for (const child of children) {
        const childTag = child.localName.toLowerCase();
        const childIndex = (tagIndexes.get(childTag) || 0) + 1;
        tagIndexes.set(childTag, childIndex);
        xpathPartCache.set(child, `${childTag}[${childIndex}]`);
      }
      for (let index = children.length - 1; index >= 0; index -= 1) {
        pending.push({el: children[index], inheritedSensitive: isSensitive});
      }
    }
  }
  if (!truncated) {
    for (let index = textEntries.length - 1; index >= 0; index -= 1) {
      const el = textEntries[index];
      if (sensitiveState.get(el)) {
        descendantTextCache.set(el, null);
        continue;
      }
      let combined = null;
      for (const child of el.childNodes) {
        if (child.nodeType === Node.TEXT_NODE) {
          combined = appendText(combined, child.nodeValue || '');
        } else if (child.nodeType === Node.ELEMENT_NODE) {
          const childElement = child;
          const tag = childElement.localName.toLowerCase();
          if (SKIPPED_TAGS.includes(tag) || childElement.hidden ||
              childElement.getAttribute('aria-hidden') === 'true' ||
              sensitiveState.get(childElement)) continue;
          combined = appendText(combined, descendantTextCache.get(childElement));
        }
        if (combined && combined.length >= MAX_TEXT) break;
      }
      descendantTextCache.set(el, combined);
    }
  }
  const directText = (el) => {
    let combined = null;
    for (const child of el.childNodes) {
      if (child.nodeType === Node.TEXT_NODE) {
        combined = appendText(combined, child.nodeValue || '');
        if (combined && combined.length >= MAX_TEXT) break;
      }
    }
    return combined;
  };
  const descendantText = (el) => sensitiveState.get(el) ? null :
    (descendantTextCache.get(el) || null);
  const referencedText = (elements) => {
    let combined = null;
    let count = 0;
    for (const element of elements) {
      if (count >= MAX_NAME_REFERENCES) break;
      count += 1;
      if (!element) continue;
      combined = appendText(combined, descendantText(element));
      if (combined && combined.length >= MAX_TEXT) break;
    }
    return combined;
  };
  const nameOf = (el) => {
    const labelledBy = referencedText((el.getAttribute('aria-labelledby') || '')
      .slice(0, MAX_TEXT).trim().split(/\s+/).filter(Boolean)
      .map((id) => document.getElementById(id)));
    const labels = el.labels ? referencedText(el.labels) : null;
    const type = lowerAttribute(el, 'type', '', 64);
    const controlValue = el.localName.toLowerCase() === 'input' &&
      ['button', 'submit', 'reset'].includes(type) ? el.value : null;
    return text(labelledBy) || text(el.getAttribute('aria-label')) || text(labels) ||
      text(el.getAttribute('alt')) || text(controlValue) ||
      text(el.getAttribute('title')) || text(el.getAttribute('placeholder')) ||
      descendantText(el);
  };
  const valueOf = (el, isSensitive) => {
    if (isSensitive) return null;
    if ('value' in el && typeof el.value !== 'undefined') return text(el.value);
    return text(el.getAttribute('value'));
  };
  const xpathPart = (el) => {
    return xpathPartCache.get(el) || `${el.localName.toLowerCase()}[1]`;
  };
  const visit = (root) => {
    const pending = [{
      el: root,
      parentPath: null,
      path: '0',
      parentXpath: '',
      inheritedSensitive: false
    }];
    while (pending.length) {
      const {el, parentPath, path, parentXpath, inheritedSensitive} = pending.pop();
      const tag = el.localName.toLowerCase();
      if (SKIPPED_TAGS.includes(tag) ||
          tag === 'input' && lowerAttribute(el, 'type', '', 64) === 'hidden' ||
          el.hidden || el.getAttribute('aria-hidden') === 'true') continue;
      if (nodes.length >= MAX_NODES) {
        truncated = true;
        return;
      }
      const rect = el.getBoundingClientRect();
      const currentXpathPart = xpathPart(el);
      if (parentXpath.length + 1 + currentXpathPart.length > MAX_XPATH) {
        truncated = true;
        return;
      }
      const xpath = `${parentXpath}/${currentXpathPart}`;
      const isSensitive = inheritedSensitive || sensitive(el);
      const disabled = el.matches(':disabled') || el.getAttribute('aria-disabled') === 'true';
      nodes.push({
        path,
        parentPath,
        xpath,
        role: implicitRole(el),
        name: isSensitive ? null : nameOf(el),
        value: valueOf(el, isSensitive),
        sensitive: isSensitive,
        identifier: isSensitive ? null : text(el.id),
        text: isSensitive ? null : directText(el),
        bounds: {x: rect.x, y: rect.y, width: rect.width, height: rect.height},
        enabled: !disabled,
        hittable: null
      });
      const children = Array.from(el.children);
      for (let index = children.length - 1; index >= 0; index -= 1) {
        const childPathSuffix = `.${index}`;
        if (path.length + childPathSuffix.length > MAX_PATH) {
          truncated = true;
          return;
        }
        const childPath = `${path}${childPathSuffix}`;
        pending.push({
          el: children[index],
          parentPath: path,
          path: childPath,
          parentXpath: xpath,
          inheritedSensitive: isSensitive
        });
      }
    }
  };
  if (document.documentElement && !truncated) {
    visit(document.documentElement);
  }
  return {
    truncated,
    href: String(location.href),
    timeOrigin: String(performance.timeOrigin),
    documentToken,
    nodes
  };
})();
"#;

const CSS_RESOLVE_SCRIPT: &str = r#"
return (() => {
  let matches;
  try {
    matches = document.querySelectorAll(arguments[0]);
  } catch (_) {
    return {invalid: true, count: 0, path: null};
  }
  if (matches.length !== 1) {
    return {invalid: false, count: matches.length, path: null};
  }
  const indexes = [];
  let element = matches[0];
  while (element && element !== document.documentElement) {
    const parent = element.parentElement;
    if (!parent) return {invalid: false, count: 0, path: null};
    indexes.push(Array.prototype.indexOf.call(parent.children, element));
    element = parent;
  }
  indexes.push(0);
  indexes.reverse();
  return {invalid: false, count: 1, path: indexes.join('.')};
})();
"#;

#[derive(Clone, Debug)]
pub(crate) struct ElementLocator {
    pub strategy: AppiumLocatorStrategy,
    pub value: String,
}

#[derive(Clone, Debug)]
pub(crate) struct CachedSnapshot {
    pub snapshot: UiSnapshot,
    locators: HashMap<String, ElementLocator>,
    source_paths: HashMap<String, String>,
}

#[derive(Clone, Debug)]
pub(crate) struct SnapshotMaterial {
    pub cached: CachedSnapshot,
    pub viewport: Viewport,
    pub source_format: &'static str,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedNode {
    pub node: UiNodeRef,
    pub locator: ElementLocator,
}

impl ResolvedNode {
    pub async fn find(
        &self,
        transport: &dyn AppiumTransport,
        session: &AppiumSession,
        control: &ExecutionControl,
    ) -> DriverResult<AppiumElement> {
        transport
            .find_element(session, self.locator.strategy, &self.locator.value, control)
            .await
    }
}

pub(crate) async fn capture_snapshot(
    transport: &dyn AppiumTransport,
    session: &AppiumSession,
    observation_id: Uuid,
    session_generation: u64,
    control: &ExecutionControl,
) -> DriverResult<SnapshotMaterial> {
    let context = transport.current_context(session, control).await?;
    capture_snapshot_in_context(
        transport,
        session,
        &context,
        observation_id,
        session_generation,
        control,
    )
    .await
}

pub(crate) async fn capture_snapshot_in_context(
    transport: &dyn AppiumTransport,
    session: &AppiumSession,
    context: &AppiumContext,
    observation_id: Uuid,
    session_generation: u64,
    control: &ExecutionControl,
) -> DriverResult<SnapshotMaterial> {
    let viewport = transport.viewport(session, control).await?;
    let (cached, source_format) = if context.is_native() {
        let source = transport.native_source_json(session, control).await?;
        (
            normalize_native(observation_id, session_generation, context, source)?,
            "json",
        )
    } else {
        let source = transport
            .execute_script(session, WEB_SNAPSHOT_SCRIPT, &[], control)
            .await?;
        (
            normalize_web(observation_id, session_generation, context, source)?,
            "dom",
        )
    };
    Ok(SnapshotMaterial {
        cached,
        viewport,
        source_format,
    })
}

pub(crate) async fn select_context(
    transport: &dyn AppiumTransport,
    session: &AppiumSession,
    selector: Option<&devicerail_protocol::UiContextSelector>,
    control: &ExecutionControl,
) -> DriverResult<AppiumContext> {
    let current = transport.current_context(session, control).await?;
    let Some(selector) = selector else {
        return Ok(current);
    };
    selector
        .validate()
        .map_err(|error| DriverError::Protocol(error.to_string()))?;
    let wanted_native = selector.context_kind == UiContextKind::Native;
    if current.is_native() == wanted_native
        && selector
            .context_id
            .as_deref()
            .is_none_or(|id| id == current.as_str())
    {
        return Ok(current);
    }

    let contexts = transport.contexts(session, control).await?;
    let mut matches = contexts.into_iter().filter(|candidate| {
        candidate.is_native() == wanted_native
            && selector
                .context_id
                .as_deref()
                .is_none_or(|id| id == candidate.as_str())
    });
    let selected = matches.next().ok_or(DriverError::UiContextNotFound)?;
    if matches.next().is_some() {
        return Err(DriverError::UiContextAmbiguous);
    }
    transport
        .switch_context(session, &selected, control)
        .await?;
    Ok(selected)
}

pub(crate) async fn resolve_selector(
    transport: &dyn AppiumTransport,
    session: &AppiumSession,
    cached: &CachedSnapshot,
    selector: &ElementSelector,
    control: &ExecutionControl,
) -> DriverResult<ResolvedNode> {
    selector
        .validate()
        .map_err(|error| DriverError::Protocol(error.to_string()))?;
    verify_selector_context(selector, &cached.snapshot.context)?;
    if let Some(css) = &selector.css {
        let result = transport
            .execute_script(
                session,
                CSS_RESOLVE_SCRIPT,
                &[Value::String(css.clone())],
                control,
            )
            .await?;
        let result = result
            .as_object()
            .ok_or_else(|| platform_protocol("invalid CSS resolver result"))?;
        if result.get("invalid").and_then(Value::as_bool) != Some(false) {
            return Err(DriverError::InvalidArguments {
                action: "findElement".to_owned(),
                message: "css is not a valid selector".to_owned(),
            });
        }
        let count = result
            .get("count")
            .and_then(Value::as_u64)
            .ok_or_else(|| platform_protocol("invalid CSS resolver count"))?;
        match count {
            0 => return Err(DriverError::ElementNotFound),
            1 => {}
            _ => return Err(DriverError::ElementAmbiguous),
        }
        let path =
            bounded_required_string(result.get("path"), MAX_UI_IDENTIFIER_LENGTH, "css path")?;
        let stable_id = cached
            .source_paths
            .get(&path)
            .cloned()
            .ok_or(DriverError::UiContextChanged)?;
        return Ok(ResolvedNode {
            node: node_ref(cached, &stable_id),
            locator: ElementLocator {
                strategy: AppiumLocatorStrategy::CssSelector,
                value: css.clone(),
            },
        });
    }

    let mut matches = cached
        .snapshot
        .nodes
        .iter()
        .filter(|node| selector_matches(selector, node));
    let node = matches.next().ok_or(DriverError::ElementNotFound)?;
    if matches.next().is_some() {
        return Err(DriverError::ElementAmbiguous);
    }
    let locator = cached
        .locators
        .get(&node.stable_node_id)
        .cloned()
        .ok_or_else(|| platform_protocol("UI node has no locator"))?;
    Ok(ResolvedNode {
        node: node_ref(cached, &node.stable_node_id),
        locator,
    })
}

pub(crate) fn resolve_target(
    cached: &CachedSnapshot,
    target: &ElementTarget,
) -> DriverResult<Option<ResolvedNode>> {
    target
        .validate()
        .map_err(|error| DriverError::Protocol(error.to_string()))?;
    let ElementTarget::Node { node } = target else {
        return Ok(None);
    };
    if node.context != cached.snapshot.context {
        return Err(DriverError::ElementStale);
    }
    let locator = cached
        .locators
        .get(&node.stable_node_id)
        .cloned()
        .ok_or(DriverError::ElementStale)?;
    Ok(Some(ResolvedNode {
        node: node_ref(cached, &node.stable_node_id),
        locator,
    }))
}

pub(crate) fn validate_target_provenance(
    cached: Option<&CachedSnapshot>,
    target: &ElementTarget,
) -> DriverResult<()> {
    let ElementTarget::Node { node } = target else {
        return Ok(());
    };
    let cached = cached.ok_or(DriverError::ElementStale)?;
    if node.observation_id != cached.snapshot.observation_id
        || node.context != cached.snapshot.context
        || !cached.locators.contains_key(&node.stable_node_id)
    {
        return Err(DriverError::ElementStale);
    }
    Ok(())
}

pub(crate) fn target_context(
    target: &ElementTarget,
) -> Option<devicerail_protocol::UiContextSelector> {
    match target {
        ElementTarget::Selector { selector } => selector.context.clone(),
        ElementTarget::Node { node } => Some(devicerail_protocol::UiContextSelector {
            context_kind: node.context.context_kind,
            context_id: Some(node.context.context_id.clone()),
        }),
    }
}

pub(crate) fn normalize_native(
    observation_id: Uuid,
    session_generation: u64,
    context: &AppiumContext,
    source: Value,
) -> DriverResult<CachedSnapshot> {
    let encoded = serde_json::to_vec(&source)
        .map_err(|_| platform_protocol("could not serialize native source"))?;
    if encoded.is_empty() || encoded.len() > MAX_NATIVE_SOURCE_BYTES {
        return Err(platform_protocol(
            "native source exceeds the bounded contract",
        ));
    }
    let source = unwrap_source(source);
    let roots: Vec<&Value> = match &source {
        Value::Array(values) => values.iter().collect(),
        Value::Object(_) => vec![&source],
        _ => return Err(platform_protocol("native source is not a UI tree")),
    };
    if roots.is_empty() {
        return Err(platform_protocol("native source has no root"));
    }
    let root_identity = native_root_identity(&roots)?;
    let mut nodes = Vec::new();
    let mut root_ids = Vec::new();
    let mut locators = HashMap::new();
    for (index, root) in roots.into_iter().enumerate() {
        let stable_id = format!("native:{index}");
        root_ids.push(stable_id.clone());
        walk_native(
            root,
            None,
            &stable_id,
            &format!("/*[{}]", index + 1),
            false,
            &mut nodes,
            &mut locators,
        )?;
    }
    // Accessibility identifiers are the preferred native locator, but XCTest
    // does not guarantee they are unique. Preserve the structural XPath for
    // duplicates so a normalized node can never resolve to a different
    // element merely because two controls share the same identifier.
    let mut identifier_counts = HashMap::new();
    for identifier in nodes.iter().filter_map(|node| node.identifier.as_ref()) {
        *identifier_counts
            .entry(identifier.clone())
            .or_insert(0_usize) += 1;
    }
    let renames = apply_identity_ids(
        "native",
        observation_id,
        &mut nodes,
        &mut root_ids,
        &mut locators,
    );
    for node in &nodes {
        let Some(identifier) = node.identifier.as_ref() else {
            continue;
        };
        if identifier_counts.get(identifier) == Some(&1) {
            locators.insert(
                node.stable_node_id.clone(),
                ElementLocator {
                    strategy: AppiumLocatorStrategy::AccessibilityId,
                    value: identifier.clone(),
                },
            );
        }
    }
    debug_assert!(renames.values().all(|id| locators.contains_key(id)));
    let structure_identity = identity_structure_fingerprint(&nodes);
    let context_ref = UiContextRef {
        context_kind: UiContextKind::Native,
        context_id: context.as_str().to_owned(),
        document_epoch: document_epoch(
            session_generation,
            context.as_str(),
            &[root_identity.as_str(), structure_identity.as_str()],
        ),
    };
    finish_snapshot(
        observation_id,
        context_ref,
        root_ids,
        nodes,
        locators,
        HashMap::new(),
    )
}

fn native_root_identity(roots: &[&Value]) -> DriverResult<String> {
    let mut identities = Vec::with_capacity(roots.len());
    for (index, root) in roots.iter().enumerate() {
        let object = root
            .as_object()
            .ok_or_else(|| platform_protocol("native UI root is not an object"))?;
        let field = |names: &[&str]| {
            names
                .iter()
                .find_map(|name| object.get(*name)?.as_str())
                .unwrap_or("")
        };
        let raw_role = field(&["type", "className", "role"]);
        let identity = if native_node_is_sensitive(object, raw_role, false)? {
            ""
        } else {
            field(&["identifier", "rawIdentifier", "name"])
        };
        identities.push(format!("{index}:{raw_role}:{identity}"));
    }
    Ok(identities.join("\0"))
}

fn native_node_is_sensitive(
    object: &Map<String, Value>,
    raw_role: &str,
    inherited_sensitive: bool,
) -> DriverResult<bool> {
    Ok(inherited_sensitive
        || raw_role.eq_ignore_ascii_case("XCUIElementTypeSecureTextField")
        || raw_role.eq_ignore_ascii_case("securetextfield")
        || optional_bool(object.get("isSecureTextEntry"), "isSecureTextEntry")? == Some(true)
        || optional_bool(object.get("secure"), "secure")? == Some(true)
        || native_content_type_is_sensitive(object)?)
}

fn native_content_type_is_sensitive(object: &Map<String, Value>) -> DriverResult<bool> {
    for key in [
        "textContentType",
        "contentType",
        "autocomplete",
        "autoComplete",
        "autofillType",
        "inputType",
        "accessibilityTextualContext",
    ] {
        let Some(content_type) =
            optional_scalar_string(object.get(key), MAX_NATIVE_CONTENT_TYPE_LENGTH, key)?
        else {
            continue;
        };
        let normalized = content_type
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>();
        if normalized == "otp"
            || normalized.ends_with("onetimecode")
            || normalized.ends_with("currentpassword")
            || normalized.ends_with("newpassword")
            || normalized.ends_with("verificationcode")
            || normalized.ends_with("securitycode")
            || normalized.ends_with("passcode")
            || normalized.ends_with("smsotp")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn walk_native(
    value: &Value,
    parent: Option<&str>,
    stable_id: &str,
    xpath: &str,
    inherited_sensitive: bool,
    nodes: &mut Vec<UiNode>,
    locators: &mut HashMap<String, ElementLocator>,
) -> DriverResult<()> {
    if nodes.len() >= MAX_UI_SNAPSHOT_NODES {
        return Err(platform_protocol("native UI tree exceeds the node limit"));
    }
    let object = value
        .as_object()
        .ok_or_else(|| platform_protocol("native UI node is not an object"))?;
    let raw_role =
        optional_string_from_keys(object, &["type", "className", "role"], MAX_UI_ROLE_LENGTH)?
            .unwrap_or_else(|| "unknown".to_owned());
    let role = normalize_native_role(&raw_role);
    let raw_name =
        optional_string_from_keys(object, &["label", "title", "name"], MAX_UI_TEXT_LENGTH)?;
    let sensitive = native_node_is_sensitive(object, &raw_role, inherited_sensitive)?;
    let name = (!sensitive).then_some(raw_name).flatten();
    let value_text = if sensitive {
        None
    } else {
        optional_scalar_string(object.get("value"), MAX_UI_TEXT_LENGTH, "value")?
    };
    let identifier = if sensitive {
        None
    } else {
        optional_string_from_keys(
            object,
            &["identifier", "rawIdentifier"],
            MAX_UI_IDENTIFIER_LENGTH,
        )?
        .or_else(|| {
            object
                .get("name")
                .and_then(Value::as_str)
                .filter(|candidate| Some(*candidate) != name.as_deref())
                .map(str::to_owned)
        })
    };
    if identifier
        .as_deref()
        .is_some_and(|value| value.chars().count() > MAX_UI_IDENTIFIER_LENGTH)
    {
        return Err(platform_protocol(
            "native identifier exceeds the wire limit",
        ));
    }
    let text = if sensitive {
        None
    } else {
        optional_string_from_keys(object, &["label", "text"], MAX_UI_TEXT_LENGTH)?
            .or_else(|| value_text.clone())
    };
    let bounds = parse_bounds(object)?;
    let enabled = optional_bool(object.get("enabled"), "enabled")?;
    let hittable = optional_bool(object.get("hittable"), "hittable")?;
    let node = UiNode {
        stable_node_id: stable_id.to_owned(),
        parent_stable_node_id: parent.map(str::to_owned),
        role,
        name,
        value: value_text,
        identifier: identifier.clone(),
        text,
        bounds,
        enabled,
        hittable,
    };
    node.validate()
        .map_err(|error| platform_protocol(&error.to_string()))?;
    nodes.push(node);
    locators.insert(
        stable_id.to_owned(),
        ElementLocator {
            strategy: AppiumLocatorStrategy::XPath,
            value: xpath.to_owned(),
        },
    );

    let children = object
        .get("children")
        .map(|value| {
            value
                .as_array()
                .ok_or_else(|| platform_protocol("native children is not an array"))
        })
        .transpose()?
        .map(Vec::as_slice)
        .unwrap_or_default();
    for (index, child) in children.iter().enumerate() {
        let (child_id, child_xpath) = bounded_native_child_paths(stable_id, xpath, index)?;
        walk_native(
            child,
            Some(stable_id),
            &child_id,
            &child_xpath,
            sensitive,
            nodes,
            locators,
        )?;
    }
    Ok(())
}

fn bounded_native_child_paths(
    stable_id: &str,
    xpath: &str,
    index: usize,
) -> DriverResult<(String, String)> {
    let ordinal = index
        .checked_add(1)
        .ok_or_else(|| platform_protocol("native child index overflow"))?
        .to_string();
    let index = index.to_string();
    if stable_id
        .len()
        .checked_add(1 + index.len())
        .is_none_or(|length| length > MAX_UI_IDENTIFIER_LENGTH)
    {
        return Err(platform_protocol(
            "native structural id exceeds the wire limit",
        ));
    }
    if xpath
        .len()
        .checked_add(4 + ordinal.len())
        .is_none_or(|length| length > MAX_LOCATOR_CHARS)
    {
        return Err(platform_protocol("native XPath exceeds the locator limit"));
    }
    Ok((
        format!("{stable_id}.{index}"),
        format!("{xpath}/*[{ordinal}]"),
    ))
}

fn normalize_web(
    observation_id: Uuid,
    session_generation: u64,
    context: &AppiumContext,
    source: Value,
) -> DriverResult<CachedSnapshot> {
    let encoded = serde_json::to_vec(&source)
        .map_err(|_| platform_protocol("could not serialize DOM snapshot"))?;
    if encoded.is_empty() || encoded.len() > MAX_WEB_RESULT_BYTES {
        return Err(platform_protocol(
            "DOM snapshot exceeds the bounded contract",
        ));
    }
    let object = source
        .as_object()
        .ok_or_else(|| platform_protocol("DOM snapshot is not an object"))?;
    if object.get("truncated").and_then(Value::as_bool) != Some(false) {
        return Err(platform_protocol("DOM snapshot exceeds the node limit"));
    }
    let raw_nodes = object
        .get("nodes")
        .and_then(Value::as_array)
        .filter(|nodes| !nodes.is_empty() && nodes.len() <= MAX_UI_SNAPSHOT_NODES)
        .ok_or_else(|| platform_protocol("DOM snapshot has no bounded root"))?;
    let href = bounded_required_string(object.get("href"), MAX_UI_TEXT_LENGTH, "href")?;
    let time_origin = bounded_required_string(
        object.get("timeOrigin"),
        MAX_UI_IDENTIFIER_LENGTH,
        "timeOrigin",
    )?;
    let document_token = bounded_required_string(
        object.get("documentToken"),
        MAX_UI_IDENTIFIER_LENGTH,
        "documentToken",
    )?;
    let navigation_identity = format!("{href}\0{time_origin}\0{document_token}");
    let mut nodes = Vec::with_capacity(raw_nodes.len());
    let mut root_ids = Vec::new();
    let mut locators = HashMap::with_capacity(raw_nodes.len());
    for raw in raw_nodes {
        let raw = raw
            .as_object()
            .ok_or_else(|| platform_protocol("DOM node is not an object"))?;
        let path = bounded_required_string(raw.get("path"), MAX_WEB_PATH_LENGTH, "path")?;
        let stable_id = format!("web:{path}");
        let parent = match raw.get("parentPath") {
            None | Some(Value::Null) => {
                root_ids.push(stable_id.clone());
                None
            }
            value => Some(format!(
                "web:{}",
                bounded_required_string(value, MAX_UI_IDENTIFIER_LENGTH - 4, "parentPath")?
            )),
        };
        let role = bounded_required_string(raw.get("role"), MAX_UI_ROLE_LENGTH, "role")?;
        let sensitive = optional_bool(raw.get("sensitive"), "sensitive")? == Some(true);
        let node = UiNode {
            stable_node_id: stable_id.clone(),
            parent_stable_node_id: parent,
            role,
            name: if sensitive {
                None
            } else {
                optional_bounded_string(raw.get("name"), MAX_UI_TEXT_LENGTH, "name")?
            },
            value: if sensitive {
                None
            } else {
                optional_bounded_string(raw.get("value"), MAX_UI_TEXT_LENGTH, "value")?
            },
            identifier: if sensitive {
                None
            } else {
                optional_bounded_string(
                    raw.get("identifier"),
                    MAX_UI_IDENTIFIER_LENGTH,
                    "identifier",
                )?
            },
            text: if sensitive {
                None
            } else {
                optional_bounded_string(raw.get("text"), MAX_UI_TEXT_LENGTH, "text")?
            },
            bounds: parse_rect_value(raw.get("bounds"))?,
            enabled: optional_bool(raw.get("enabled"), "enabled")?,
            hittable: optional_bool(raw.get("hittable"), "hittable")?,
        };
        node.validate()
            .map_err(|error| platform_protocol(&error.to_string()))?;
        let xpath = bounded_required_string(raw.get("xpath"), MAX_WEB_XPATH_LENGTH, "xpath")?;
        locators.insert(
            stable_id,
            ElementLocator {
                strategy: AppiumLocatorStrategy::XPath,
                value: xpath,
            },
        );
        nodes.push(node);
    }
    let renames = apply_identity_ids(
        "web",
        observation_id,
        &mut nodes,
        &mut root_ids,
        &mut locators,
    );
    let structure_identity = identity_structure_fingerprint(&nodes);
    let context_ref = UiContextRef {
        context_kind: UiContextKind::Web,
        context_id: context.as_str().to_owned(),
        document_epoch: document_epoch(
            session_generation,
            context.as_str(),
            &[navigation_identity.as_str(), structure_identity.as_str()],
        ),
    };
    let source_paths = raw_nodes
        .iter()
        .filter_map(|raw| raw.get("path").and_then(Value::as_str))
        .map(|path| {
            let structural = format!("web:{path}");
            (
                path.to_owned(),
                renames.get(&structural).cloned().unwrap_or(structural),
            )
        })
        .collect();
    finish_snapshot(
        observation_id,
        context_ref,
        root_ids,
        nodes,
        locators,
        source_paths,
    )
}

fn apply_identity_ids(
    prefix: &str,
    observation_id: Uuid,
    nodes: &mut [UiNode],
    roots: &mut [String],
    locators: &mut HashMap<String, ElementLocator>,
) -> HashMap<String, String> {
    let mut counts = HashMap::new();
    for identifier in nodes.iter().filter_map(|node| node.identifier.as_ref()) {
        *counts.entry(identifier.clone()).or_insert(0_usize) += 1;
    }
    let mut structural_counts = HashMap::new();
    for node in nodes.iter() {
        if node
            .identifier
            .as_ref()
            .is_some_and(|identifier| counts.get(identifier) == Some(&1))
        {
            continue;
        }
        *structural_counts
            .entry(structural_disambiguation_key(node))
            .or_insert(0_usize) += 1;
    }
    let mut renames = HashMap::new();
    for node in nodes.iter() {
        let identity = node
            .identifier
            .as_deref()
            .filter(|identifier| counts.get(*identifier) == Some(&1))
            .map_or_else(
                || {
                    let key = structural_disambiguation_key(node);
                    let ambiguous = structural_counts.get(&key) != Some(&1);
                    stable_structural_identity(prefix, node, ambiguous.then_some(observation_id))
                },
                |identifier| stable_identity(prefix, identifier),
            );
        renames.insert(node.stable_node_id.clone(), identity);
    }
    for node in nodes.iter_mut() {
        if let Some(id) = renames.get(&node.stable_node_id) {
            node.stable_node_id = id.clone();
        }
        if let Some(parent) = node.parent_stable_node_id.as_mut()
            && let Some(id) = renames.get(parent)
        {
            *parent = id.clone();
        }
    }
    for root in roots {
        if let Some(id) = renames.get(root) {
            *root = id.clone();
        }
    }
    for (old, new) in &renames {
        if let Some(locator) = locators.remove(old) {
            locators.insert(new.clone(), locator);
        }
    }
    renames
}

fn stable_identity(prefix: &str, identifier: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(identifier.as_bytes());
    format!("{prefix}:id:{}", hex::encode(hash.finalize()))
}

fn structural_disambiguation_key(node: &UiNode) -> String {
    let mut hash = Sha256::new();
    hash_field(
        &mut hash,
        node.parent_stable_node_id.as_deref().unwrap_or_default(),
    );
    hash_field(&mut hash, &node.role);
    hash_field(&mut hash, node.identifier.as_deref().unwrap_or_default());
    hash_field(&mut hash, node.name.as_deref().unwrap_or_default());
    hex::encode(hash.finalize())
}

fn stable_structural_identity(prefix: &str, node: &UiNode, ambiguity_salt: Option<Uuid>) -> String {
    let mut hash = Sha256::new();
    hash_field(&mut hash, &node.stable_node_id);
    hash_field(
        &mut hash,
        node.parent_stable_node_id.as_deref().unwrap_or_default(),
    );
    hash_field(&mut hash, &node.role);
    hash_field(&mut hash, node.identifier.as_deref().unwrap_or_default());
    hash_field(&mut hash, node.name.as_deref().unwrap_or_default());
    if let Some(observation_id) = ambiguity_salt {
        hash.update(observation_id.as_bytes());
    }
    format!("{prefix}:node:{}", hex::encode(hash.finalize()))
}

fn identity_structure_fingerprint(nodes: &[UiNode]) -> String {
    let mut hash = Sha256::new();
    hash.update(nodes.len().to_be_bytes());
    for node in nodes {
        // Preorder plus the parent id captures ancestry and sibling order. Do
        // not include mutable values, geometry, or interaction state: those
        // changes must not invalidate an otherwise stable node reference.
        hash_field(&mut hash, &node.stable_node_id);
        hash_field(
            &mut hash,
            node.parent_stable_node_id.as_deref().unwrap_or_default(),
        );
        hash_field(&mut hash, &node.role);
        hash_field(&mut hash, node.identifier.as_deref().unwrap_or_default());
        hash_field(&mut hash, node.name.as_deref().unwrap_or_default());
    }
    format!("sha256:{}", hex::encode(hash.finalize()))
}

fn hash_field(hash: &mut Sha256, value: &str) {
    hash.update(value.len().to_be_bytes());
    hash.update(value.as_bytes());
}

fn finish_snapshot(
    observation_id: Uuid,
    context: UiContextRef,
    root_stable_node_ids: Vec<String>,
    nodes: Vec<UiNode>,
    locators: HashMap<String, ElementLocator>,
    source_paths: HashMap<String, String>,
) -> DriverResult<CachedSnapshot> {
    let snapshot = UiSnapshot {
        format_version: UI_SNAPSHOT_FORMAT_VERSION,
        observation_id,
        context,
        root_stable_node_ids,
        nodes,
    };
    snapshot
        .validate()
        .map_err(|error| platform_protocol(&error.to_string()))?;
    Ok(CachedSnapshot {
        snapshot,
        locators,
        source_paths,
    })
}

fn unwrap_source(mut source: Value) -> Value {
    if let Value::Object(object) = &mut source
        && object.len() == 1
        && let Some(value) = object.remove("value")
        && matches!(&value, Value::Object(_) | Value::Array(_))
    {
        return value;
    }
    source
}

fn node_ref(cached: &CachedSnapshot, stable_id: &str) -> UiNodeRef {
    UiNodeRef {
        observation_id: cached.snapshot.observation_id,
        context: cached.snapshot.context.clone(),
        stable_node_id: stable_id.to_owned(),
    }
}

fn verify_selector_context(selector: &ElementSelector, actual: &UiContextRef) -> DriverResult<()> {
    if let Some(expected) = &selector.context
        && (expected.context_kind != actual.context_kind
            || expected
                .context_id
                .as_deref()
                .is_some_and(|id| id != actual.context_id))
    {
        return Err(DriverError::UiContextChanged);
    }
    Ok(())
}

fn selector_matches(selector: &ElementSelector, node: &UiNode) -> bool {
    selector
        .role
        .as_deref()
        .is_none_or(|role| role.eq_ignore_ascii_case(&node.role))
        && selector
            .name
            .as_deref()
            .is_none_or(|name| node.name.as_deref() == Some(name))
        && selector
            .value
            .as_deref()
            .is_none_or(|value| node.value.as_deref() == Some(value))
        && selector
            .identifier
            .as_deref()
            .is_none_or(|identifier| node.identifier.as_deref() == Some(identifier))
        && selector.text.as_ref().is_none_or(|expected| {
            node.text.as_deref().is_some_and(|actual| {
                let (actual, expected_value) = if expected.case_sensitive {
                    (actual.to_owned(), expected.value.clone())
                } else {
                    (actual.to_lowercase(), expected.value.to_lowercase())
                };
                match expected.mode {
                    TextMatchMode::Exact => actual == expected_value,
                    TextMatchMode::Contains => actual.contains(&expected_value),
                }
            })
        })
}

fn normalize_native_role(raw: &str) -> String {
    let role = raw.strip_prefix("XCUIElementType").unwrap_or(raw);
    match role.to_ascii_lowercase().as_str() {
        "application" => "application".to_owned(),
        "button" => "button".to_owned(),
        "link" => "link".to_owned(),
        "image" => "image".to_owned(),
        "statictext" => "text".to_owned(),
        "textfield" | "securetextfield" | "textview" => "textbox".to_owned(),
        "switch" => "switch".to_owned(),
        "slider" => "slider".to_owned(),
        "cell" => "cell".to_owned(),
        "table" | "collectionview" => "list".to_owned(),
        "navigationbar" => "navigation".to_owned(),
        "alert" => "alert".to_owned(),
        "window" => "window".to_owned(),
        other if !other.is_empty() => other.to_owned(),
        _ => "unknown".to_owned(),
    }
}

fn parse_bounds(object: &Map<String, Value>) -> DriverResult<Option<UiRect>> {
    if let Some(value) = object.get("rect").or_else(|| object.get("frame")) {
        return parse_rect_value(Some(value));
    }
    if ["x", "y", "width", "height"]
        .iter()
        .all(|key| object.contains_key(*key))
    {
        return parse_rect_object(object).map(Some);
    }
    Ok(None)
}

fn parse_rect_value(value: Option<&Value>) -> DriverResult<Option<UiRect>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(object)) => parse_rect_object(object).map(Some),
        _ => Err(platform_protocol("UI bounds is not an object")),
    }
}

fn parse_rect_object(object: &Map<String, Value>) -> DriverResult<UiRect> {
    let number = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite())
            .ok_or_else(|| platform_protocol("UI bounds contains a non-finite value"))
    };
    let rect = UiRect {
        x: number("x")?,
        y: number("y")?,
        width: number("width")?,
        height: number("height")?,
    };
    if !rect.is_valid() {
        return Err(platform_protocol("UI bounds is invalid"));
    }
    Ok(rect)
}

fn optional_bool(value: Option<&Value>, field: &str) -> DriverResult<Option<bool>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(Value::String(value)) if value.eq_ignore_ascii_case("true") => Ok(Some(true)),
        Some(Value::String(value)) if value.eq_ignore_ascii_case("false") => Ok(Some(false)),
        _ => Err(platform_protocol(&format!("{field} is not boolean"))),
    }
}

fn optional_string_from_keys(
    object: &Map<String, Value>,
    keys: &[&str],
    max_chars: usize,
) -> DriverResult<Option<String>> {
    for key in keys {
        if let Some(value) = object.get(*key) {
            let parsed = optional_bounded_string(Some(value), max_chars, key)?;
            if parsed.as_deref().is_some_and(|value| !value.is_empty()) {
                return Ok(parsed);
            }
        }
    }
    Ok(None)
}

fn optional_scalar_string(
    value: Option<&Value>,
    max_chars: usize,
    field: &str,
) -> DriverResult<Option<String>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => bounded_string(value, max_chars, field).map(Some),
        Some(Value::Bool(value)) => Ok(Some(value.to_string())),
        Some(Value::Number(value)) => Ok(Some(value.to_string())),
        _ => Err(platform_protocol(&format!("{field} is not scalar"))),
    }
}

fn optional_bounded_string(
    value: Option<&Value>,
    max_chars: usize,
    field: &str,
) -> DriverResult<Option<String>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => bounded_string(value, max_chars, field).map(Some),
        _ => Err(platform_protocol(&format!("{field} is not a string"))),
    }
}

fn bounded_required_string(
    value: Option<&Value>,
    max_chars: usize,
    field: &str,
) -> DriverResult<String> {
    let value = value
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| platform_protocol(&format!("{field} is missing")))?;
    bounded_string(value, max_chars, field)
}

fn bounded_string(value: &str, max_chars: usize, field: &str) -> DriverResult<String> {
    if value.chars().count() > max_chars {
        return Err(platform_protocol(&format!(
            "{field} exceeds the wire limit"
        )));
    }
    Ok(value.to_owned())
}

fn document_epoch(generation: u64, context: &str, identities: &[&str]) -> String {
    let mut hash = Sha256::new();
    hash.update(generation.to_be_bytes());
    hash_field(&mut hash, context);
    for identity in identities {
        hash_field(&mut hash, identity);
    }
    format!("sha256:{}", hex::encode(hash.finalize()))
}

fn platform_protocol(message: &str) -> DriverError {
    DriverError::Protocol(format!("invalid Appium UI source: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_json_normalizes_preorder_and_accessibility_locators() {
        let context = AppiumContext::native();
        let cached = normalize_native(
            Uuid::nil(),
            1,
            &context,
            json!({
                "type": "XCUIElementTypeApplication",
                "enabled": true,
                "children": [{
                    "type": "XCUIElementTypeButton",
                    "label": "Search",
                    "identifier": "search-button",
                    "rect": {"x": 1, "y": 2, "width": 30, "height": 40},
                    "enabled": true,
                    "hittable": true,
                    "children": []
                }]
            }),
        )
        .expect("normalized native tree");
        assert_eq!(cached.snapshot.nodes.len(), 2);
        assert_eq!(cached.snapshot.nodes[1].role, "button");
        assert_eq!(
            cached
                .locators
                .get(&cached.snapshot.nodes[1].stable_node_id)
                .expect("button locator")
                .strategy,
            AppiumLocatorStrategy::AccessibilityId
        );
    }

    #[test]
    fn selector_matching_is_explicit_and_ambiguous_matches_fail() {
        let context = AppiumContext::native();
        let cached = normalize_native(
            Uuid::nil(),
            1,
            &context,
            json!({
                "type": "XCUIElementTypeApplication",
                "children": [
                    {"type": "XCUIElementTypeButton", "label": "Save", "children": []},
                    {"type": "XCUIElementTypeButton", "label": "Save", "children": []}
                ]
            }),
        )
        .expect("normalized native tree");
        let selector = ElementSelector {
            role: Some("button".to_owned()),
            name: Some("Save".to_owned()),
            ..ElementSelector::default()
        };
        let mut matches = cached
            .snapshot
            .nodes
            .iter()
            .filter(|node| selector_matches(&selector, node));
        assert!(matches.next().is_some());
        assert!(matches.next().is_some());
    }

    #[test]
    fn duplicate_native_identifiers_fall_back_to_structural_xpath() {
        let context = AppiumContext::native();
        let cached = normalize_native(
            Uuid::nil(),
            1,
            &context,
            json!({
                "type": "XCUIElementTypeApplication",
                "children": [
                    {"type": "XCUIElementTypeButton", "identifier": "duplicate", "children": []},
                    {"type": "XCUIElementTypeButton", "identifier": "duplicate", "children": []}
                ]
            }),
        )
        .expect("normalized native tree");
        let first_id = &cached.snapshot.nodes[1].stable_node_id;
        let second_id = &cached.snapshot.nodes[2].stable_node_id;

        assert_eq!(
            cached.locators[first_id].strategy,
            AppiumLocatorStrategy::XPath
        );
        assert_eq!(
            cached.locators[second_id].strategy,
            AppiumLocatorStrategy::XPath
        );
        assert_ne!(
            cached.locators[first_id].value,
            cached.locators[second_id].value
        );

        let refreshed = normalize_native(
            Uuid::new_v4(),
            1,
            &context,
            json!({
                "type": "XCUIElementTypeApplication",
                "children": [
                    {"type": "XCUIElementTypeButton", "identifier": "duplicate", "children": []},
                    {"type": "XCUIElementTypeButton", "identifier": "duplicate", "children": []}
                ]
            }),
        )
        .expect("refreshed ambiguous native tree");
        let target = ElementTarget::Node {
            node: node_ref(&cached, first_id),
        };
        assert!(matches!(
            resolve_target(&refreshed, &target),
            Err(DriverError::ElementStale)
        ));
    }

    #[test]
    fn web_snapshot_rejects_truncation_instead_of_returning_a_partial_tree() {
        let context = AppiumContext::parse("WEBVIEW_1").expect("web context");
        assert!(
            normalize_web(
                Uuid::nil(),
                1,
                &context,
                json!({"truncated": true, "href": "https://example.test", "timeOrigin": "1", "documentToken": "document-1", "nodes": []}),
            )
            .is_err()
        );
    }

    #[test]
    fn sensitive_values_are_redacted_and_never_reach_the_snapshot() {
        const SECRET: &str = "SENTINEL-PASSWORD-DO-NOT-PERSIST";
        let native = normalize_native(
            Uuid::nil(),
            7,
            &AppiumContext::native(),
            json!({
                "type": "XCUIElementTypeSecureTextField",
                "label": SECRET,
                "name": SECRET,
                "identifier": SECRET,
                "value": SECRET,
                "text": SECRET,
                "children": [{
                    "type": "XCUIElementTypeStaticText",
                    "label": SECRET,
                    "name": SECRET,
                    "identifier": SECRET,
                    "value": SECRET,
                    "text": SECRET,
                    "children": []
                }]
            }),
        )
        .expect("secure native snapshot");
        let native_json = serde_json::to_string(&native.snapshot).expect("native JSON");
        assert!(!native_json.contains(SECRET));
        assert!(native.snapshot.nodes.iter().all(|node| {
            node.name.is_none()
                && node.value.is_none()
                && node.identifier.is_none()
                && node.text.is_none()
        }));

        let web = normalize_web(
            Uuid::nil(),
            7,
            &AppiumContext::parse("WEBVIEW_1").expect("web context"),
            json!({
                "truncated": false,
                "href": "https://example.test/login",
                "timeOrigin": "123",
                "documentToken": "document-1",
                "nodes": [
                    {
                        "path": "0",
                        "parentPath": null,
                        "xpath": "/html[1]",
                        "role": "generic",
                        "name": null,
                        "value": null,
                        "text": null,
                        "sensitive": false,
                        "identifier": null,
                        "bounds": {"x": 0, "y": 0, "width": 10, "height": 10},
                        "enabled": true,
                        "hittable": null
                    },
                    {
                        "path": "0.0",
                        "parentPath": "0",
                        "xpath": "/html[1]/div[1]",
                        "role": "textbox",
                        "name": SECRET,
                        "value": SECRET,
                        "text": SECRET,
                        "sensitive": true,
                        "identifier": SECRET,
                        "bounds": {"x": 0, "y": 0, "width": 10, "height": 10},
                        "enabled": true,
                        "hittable": null
                    }
                ]
            }),
        )
        .expect("secure web snapshot");
        let web_json = serde_json::to_string(&web.snapshot).expect("web JSON");
        assert!(!web_json.contains(SECRET));
        assert!(web.snapshot.nodes.iter().all(|node| {
            node.name.is_none()
                && node.value.is_none()
                && node.identifier.is_none()
                && node.text.is_none()
        }));
        assert!(!WEB_SNAPSHOT_SCRIPT.contains("el.innerText"));
        assert!(WEB_SNAPSHOT_SCRIPT.contains("type === 'password'"));
        assert!(WEB_SNAPSHOT_SCRIPT.contains("split(/[\\t\\n\\f\\r ]+/)"));
    }

    #[test]
    fn native_otp_content_types_redact_the_entire_subtree() {
        const FIRST_SECRET: &str = "173942";
        const SECOND_SECRET: &str = "825601";

        fn otp_source(key: &str, content_type: &str, secret: &str) -> Value {
            let mut source = json!({
                "type": "XCUIElementTypeTextField",
                "label": secret,
                "name": secret,
                "identifier": secret,
                "value": secret,
                "text": secret,
                "children": [{
                    "type": "XCUIElementTypeStaticText",
                    "label": secret,
                    "name": secret,
                    "identifier": secret,
                    "value": secret,
                    "text": secret,
                    "children": []
                }]
            });
            source
                .as_object_mut()
                .expect("OTP source object")
                .insert(key.to_owned(), Value::String(content_type.to_owned()));
            source
        }

        for (key, content_type) in [
            ("textContentType", "oneTimeCode"),
            ("contentType", "UITextContentTypeOneTimeCode"),
            ("autocomplete", "otp"),
            ("accessibilityTextualContext", "verificationCode"),
        ] {
            let first = normalize_native(
                Uuid::nil(),
                7,
                &AppiumContext::native(),
                otp_source(key, content_type, FIRST_SECRET),
            )
            .expect("first OTP snapshot");
            let second = normalize_native(
                Uuid::new_v4(),
                7,
                &AppiumContext::native(),
                otp_source(key, content_type, SECOND_SECRET),
            )
            .expect("second OTP snapshot");
            let encoded = serde_json::to_string(&first.snapshot).expect("OTP snapshot JSON");
            assert!(!encoded.contains(FIRST_SECRET));
            assert!(first.snapshot.nodes.iter().all(|node| {
                node.name.is_none()
                    && node.value.is_none()
                    && node.identifier.is_none()
                    && node.text.is_none()
            }));
            assert_eq!(
                first.snapshot.context.document_epoch, second.snapshot.context.document_epoch,
                "secret values must not influence persisted document identity"
            );
        }
    }

    #[test]
    fn web_descendant_text_is_cached_bounded_and_secret_aware() {
        assert!(WEB_SNAPSHOT_SCRIPT.contains("const descendantTextCache = new WeakMap()"));
        assert!(WEB_SNAPSHOT_SCRIPT.contains("textEntries.length - 1"));
        assert!(WEB_SNAPSHOT_SCRIPT.contains("inheritedSensitive || sensitive(el)"));
        assert!(WEB_SNAPSHOT_SCRIPT.contains("sensitiveState.get(childElement)"));
        assert!(WEB_SNAPSHOT_SCRIPT.contains("xpathPartCache = new WeakMap()"));
        assert!(WEB_SNAPSHOT_SCRIPT.contains("slice(0, MAX_TEXT)"));
        assert!(WEB_SNAPSHOT_SCRIPT.contains("inheritedSensitive: isSensitive"));
        assert!(WEB_SNAPSHOT_SCRIPT.contains(&format!("const MAX_PATH = {MAX_WEB_PATH_LENGTH};")));
        assert!(
            WEB_SNAPSHOT_SCRIPT.contains(&format!("const MAX_XPATH = {MAX_WEB_XPATH_LENGTH};"))
        );
        let xpath_budget = WEB_SNAPSHOT_SCRIPT
            .find("if (parentXpath.length + 1 + currentXpathPart.length > MAX_XPATH)")
            .expect("XPath budget check");
        let xpath_construction = WEB_SNAPSHOT_SCRIPT
            .find("const xpath = `${parentXpath}/${currentXpathPart}`")
            .expect("XPath construction");
        assert!(xpath_budget < xpath_construction);
        let path_budget = WEB_SNAPSHOT_SCRIPT
            .find("if (path.length + childPathSuffix.length > MAX_PATH)")
            .expect("path budget check");
        let path_construction = WEB_SNAPSHOT_SCRIPT
            .find("const childPath = `${path}${childPathSuffix}`")
            .expect("path construction");
        assert!(path_budget < path_construction);
        assert!(!WEB_SNAPSHOT_SCRIPT.contains("const collect ="));
        assert!(!WEB_SNAPSHOT_SCRIPT.contains("pieces.push"));
        assert!(!WEB_SNAPSHOT_SCRIPT.contains("previousElementSibling"));
    }

    #[test]
    fn native_structural_paths_are_bounded_before_construction() {
        let max_parent_id = "n".repeat(MAX_UI_IDENTIFIER_LENGTH - 2);
        let max_parent_xpath = "x".repeat(MAX_LOCATOR_CHARS - 5);
        let (child_id, child_xpath) =
            bounded_native_child_paths(&max_parent_id, &max_parent_xpath, 0)
                .expect("paths at exact limits");
        assert_eq!(child_id.len(), MAX_UI_IDENTIFIER_LENGTH);
        assert_eq!(child_xpath.len(), MAX_LOCATOR_CHARS);

        assert!(
            bounded_native_child_paths(
                &"n".repeat(MAX_UI_IDENTIFIER_LENGTH - 1),
                &max_parent_xpath,
                0,
            )
            .is_err()
        );
        assert!(
            bounded_native_child_paths(&max_parent_id, &"x".repeat(MAX_LOCATOR_CHARS - 4), 0,)
                .is_err()
        );
    }

    #[test]
    fn web_snapshot_script_uses_accessibility_rules_and_conservative_state() {
        assert!(WEB_SNAPSHOT_SCRIPT.contains("el.getAttribute('aria-labelledby')"));
        assert!(WEB_SNAPSHOT_SCRIPT.contains("document.getElementById(id)"));
        assert!(WEB_SNAPSHOT_SCRIPT.contains("el.labels ? referencedText(el.labels)"));
        assert!(WEB_SNAPSHOT_SCRIPT.contains("descendantText(el)"));
        assert!(WEB_SNAPSHOT_SCRIPT.contains("el.matches(':disabled')"));
        assert!(WEB_SNAPSHOT_SCRIPT.contains("hittable: null"));
        assert!(WEB_SNAPSHOT_SCRIPT.contains("Object.defineProperty(document, key"));
    }

    #[test]
    fn epoch_is_stable_for_mutable_ui_state_and_changes_with_generation_or_navigation() {
        let context = AppiumContext::native();
        let first = normalize_native(
            Uuid::new_v4(),
            11,
            &context,
            json!({"type": "XCUIElementTypeTextField", "value": "a", "children": []}),
        )
        .expect("first snapshot");
        let changed = normalize_native(
            Uuid::new_v4(),
            11,
            &context,
            json!({"type": "XCUIElementTypeTextField", "value": "b", "children": []}),
        )
        .expect("changed snapshot");
        let reconnected = normalize_native(
            Uuid::new_v4(),
            12,
            &context,
            json!({"type": "XCUIElementTypeTextField", "value": "b", "children": []}),
        )
        .expect("reconnected snapshot");
        let other_application = normalize_native(
            Uuid::new_v4(),
            11,
            &context,
            json!({
                "type": "XCUIElementTypeApplication",
                "identifier": "com.example.other",
                "children": []
            }),
        )
        .expect("other application");
        assert_eq!(
            first.snapshot.context.document_epoch,
            changed.snapshot.context.document_epoch
        );
        assert_ne!(
            changed.snapshot.context.document_epoch,
            reconnected.snapshot.context.document_epoch
        );
        assert_ne!(
            changed.snapshot.context.document_epoch,
            other_application.snapshot.context.document_epoch
        );
    }

    #[test]
    fn identity_structure_changes_make_old_node_targets_stale() {
        let context = AppiumContext::native();
        let original = normalize_native(
            Uuid::new_v4(),
            5,
            &context,
            json!({
                "type": "XCUIElementTypeApplication",
                "identifier": "com.example.app",
                "children": [
                    {"type": "XCUIElementTypeButton", "label": "Alpha", "children": []},
                    {"type": "XCUIElementTypeButton", "label": "Beta", "children": []}
                ]
            }),
        )
        .expect("original native tree");
        let original_alpha = &original.snapshot.nodes[1];
        let target = ElementTarget::Node {
            node: node_ref(&original, &original_alpha.stable_node_id),
        };

        let reordered = normalize_native(
            Uuid::new_v4(),
            5,
            &context,
            json!({
                "type": "XCUIElementTypeApplication",
                "identifier": "com.example.app",
                "children": [
                    {"type": "XCUIElementTypeButton", "label": "Beta", "children": []},
                    {"type": "XCUIElementTypeButton", "label": "Alpha", "children": []}
                ]
            }),
        )
        .expect("reordered native tree");
        assert_ne!(
            original.snapshot.context.document_epoch,
            reordered.snapshot.context.document_epoch
        );
        assert!(matches!(
            resolve_target(&reordered, &target),
            Err(DriverError::ElementStale)
        ));

        let replacement = normalize_native(
            Uuid::new_v4(),
            5,
            &context,
            json!({
                "type": "XCUIElementTypeApplication",
                "identifier": "com.example.app",
                "children": [
                    {"type": "XCUIElementTypeButton", "label": "Replacement", "children": []},
                    {"type": "XCUIElementTypeButton", "label": "Beta", "children": []}
                ]
            }),
        )
        .expect("replacement native tree");
        assert_ne!(
            original.snapshot.context.document_epoch,
            replacement.snapshot.context.document_epoch
        );
        assert!(matches!(
            resolve_target(&replacement, &target),
            Err(DriverError::ElementStale)
        ));
    }

    #[test]
    fn web_document_token_and_structure_bound_the_epoch() {
        fn source(token: &str, value: &str, names: [&str; 2]) -> Value {
            json!({
                "truncated": false,
                "href": "https://example.test/page",
                "timeOrigin": "123",
                "documentToken": token,
                "nodes": [
                    {
                        "path": "0", "parentPath": null, "xpath": "/html[1]",
                        "role": "document", "name": "Page", "value": null,
                        "text": null, "sensitive": false, "identifier": "root",
                        "bounds": null, "enabled": true, "hittable": null
                    },
                    {
                        "path": "0.0", "parentPath": "0", "xpath": "/html[1]/button[1]",
                        "role": "button", "name": names[0], "value": value,
                        "text": names[0], "sensitive": false, "identifier": null,
                        "bounds": null, "enabled": true, "hittable": null
                    },
                    {
                        "path": "0.1", "parentPath": "0", "xpath": "/html[1]/button[2]",
                        "role": "button", "name": names[1], "value": null,
                        "text": names[1], "sensitive": false, "identifier": null,
                        "bounds": null, "enabled": true, "hittable": null
                    }
                ]
            })
        }

        let context = AppiumContext::parse("WEBVIEW_1").expect("web context");
        let first = normalize_web(
            Uuid::new_v4(),
            8,
            &context,
            source("document-a", "one", ["Alpha", "Beta"]),
        )
        .expect("first web tree");
        let value_changed = normalize_web(
            Uuid::new_v4(),
            8,
            &context,
            source("document-a", "two", ["Alpha", "Beta"]),
        )
        .expect("value change");
        assert_eq!(
            first.snapshot.context.document_epoch,
            value_changed.snapshot.context.document_epoch
        );

        let reordered = normalize_web(
            Uuid::new_v4(),
            8,
            &context,
            source("document-a", "two", ["Beta", "Alpha"]),
        )
        .expect("reordered web tree");
        assert_ne!(
            first.snapshot.context.document_epoch,
            reordered.snapshot.context.document_epoch
        );

        let new_document = normalize_web(
            Uuid::new_v4(),
            8,
            &context,
            source("document-b", "one", ["Alpha", "Beta"]),
        )
        .expect("new Document");
        assert_ne!(
            first.snapshot.context.document_epoch,
            new_document.snapshot.context.document_epoch
        );

        let target = ElementTarget::Node {
            node: node_ref(&first, &first.snapshot.nodes[1].stable_node_id),
        };
        assert!(matches!(
            resolve_target(&new_document, &target),
            Err(DriverError::ElementStale)
        ));
    }

    #[test]
    fn node_target_requires_the_cached_observation_provenance() {
        let cached = normalize_native(
            Uuid::new_v4(),
            3,
            &AppiumContext::native(),
            json!({"type": "XCUIElementTypeButton", "identifier": "save", "children": []}),
        )
        .expect("snapshot");
        let node = node_ref(&cached, &cached.snapshot.nodes[0].stable_node_id);
        let target = ElementTarget::Node { node: node.clone() };
        validate_target_provenance(Some(&cached), &target).expect("current node");

        let stale = ElementTarget::Node {
            node: UiNodeRef {
                observation_id: Uuid::new_v4(),
                ..node
            },
        };
        assert!(matches!(
            validate_target_provenance(Some(&cached), &stale),
            Err(DriverError::ElementStale)
        ));
    }
}
