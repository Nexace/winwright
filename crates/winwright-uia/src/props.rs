//! Cached-property reads. One cache request fetches everything a snapshot needs in the same
//! cross-process round trip as the element itself.

use windows::Win32::UI::Accessibility::*;
use winwright_contracts::backend::UiProps;
use winwright_contracts::element::{ControlRole, ExpandState, ToggleState, UiPattern};
use winwright_contracts::geometry::PhysicalRect;

use crate::com::{OwnedVariant, platform, take_i32_safearray};

const BASE_PROPERTIES: &[UIA_PROPERTY_ID] = &[
    UIA_ControlTypePropertyId,
    UIA_NamePropertyId,
    UIA_AutomationIdPropertyId,
    UIA_ClassNamePropertyId,
    UIA_FrameworkIdPropertyId,
    UIA_HelpTextPropertyId,
    UIA_ProcessIdPropertyId,
    UIA_NativeWindowHandlePropertyId,
    UIA_RuntimeIdPropertyId,
    UIA_BoundingRectanglePropertyId,
    UIA_IsEnabledPropertyId,
    UIA_IsOffscreenPropertyId,
    UIA_HasKeyboardFocusPropertyId,
    UIA_IsKeyboardFocusablePropertyId,
    UIA_IsPasswordPropertyId,
    UIA_LabeledByPropertyId,
    UIA_IsDialogPropertyId,
    UIA_ValueIsReadOnlyPropertyId,
    UIA_ToggleToggleStatePropertyId,
    UIA_ExpandCollapseExpandCollapseStatePropertyId,
    UIA_SelectionItemIsSelectedPropertyId,
    UIA_ScrollHorizontalScrollPercentPropertyId,
    UIA_ScrollVerticalScrollPercentPropertyId,
];

const PATTERN_AVAILABILITY: &[(UIA_PROPERTY_ID, UiPattern)] = &[
    (UIA_IsInvokePatternAvailablePropertyId, UiPattern::Invoke),
    (
        UIA_IsSelectionPatternAvailablePropertyId,
        UiPattern::Selection,
    ),
    (
        UIA_IsSelectionItemPatternAvailablePropertyId,
        UiPattern::SelectionItem,
    ),
    (UIA_IsValuePatternAvailablePropertyId, UiPattern::Value),
    (
        UIA_IsRangeValuePatternAvailablePropertyId,
        UiPattern::RangeValue,
    ),
    (UIA_IsScrollPatternAvailablePropertyId, UiPattern::Scroll),
    (
        UIA_IsScrollItemPatternAvailablePropertyId,
        UiPattern::ScrollItem,
    ),
    (
        UIA_IsExpandCollapsePatternAvailablePropertyId,
        UiPattern::ExpandCollapse,
    ),
    (UIA_IsTogglePatternAvailablePropertyId, UiPattern::Toggle),
    (UIA_IsTextPatternAvailablePropertyId, UiPattern::Text),
    (UIA_IsWindowPatternAvailablePropertyId, UiPattern::Window),
    (
        UIA_IsTransformPatternAvailablePropertyId,
        UiPattern::Transform,
    ),
    (UIA_IsGridPatternAvailablePropertyId, UiPattern::Grid),
    (
        UIA_IsGridItemPatternAvailablePropertyId,
        UiPattern::GridItem,
    ),
    (UIA_IsTablePatternAvailablePropertyId, UiPattern::Table),
    (
        UIA_IsTableItemPatternAvailablePropertyId,
        UiPattern::TableItem,
    ),
    (
        UIA_IsLegacyIAccessiblePatternAvailablePropertyId,
        UiPattern::LegacyIAccessible,
    ),
    (
        UIA_IsVirtualizedItemPatternAvailablePropertyId,
        UiPattern::VirtualizedItem,
    ),
    (
        UIA_IsItemContainerPatternAvailablePropertyId,
        UiPattern::ItemContainer,
    ),
    (UIA_IsDragPatternAvailablePropertyId, UiPattern::Drag),
    (
        UIA_IsDropTargetPatternAvailablePropertyId,
        UiPattern::DropTarget,
    ),
    (
        UIA_IsTextEditPatternAvailablePropertyId,
        UiPattern::TextEdit,
    ),
];

/// Longest value string kept from a provider; snapshots truncate further for display.
const MAX_VALUE_CHARS: usize = 1_024;

pub fn cache_request(
    automation: &IUIAutomation,
    scope: TreeScope,
) -> Result<IUIAutomationCacheRequest, winwright_contracts::WinwrightError> {
    // SAFETY: plain COM calls on live interfaces owned by the worker thread.
    unsafe {
        let request = automation
            .CreateCacheRequest()
            .map_err(|e| platform("CreateCacheRequest", &e))?;
        for &id in BASE_PROPERTIES
            .iter()
            .chain(PATTERN_AVAILABILITY.iter().map(|(id, _)| id))
        {
            request
                .AddProperty(id)
                .map_err(|e| platform("CacheRequest.AddProperty", &e))?;
        }
        request
            .SetTreeScope(scope)
            .map_err(|e| platform("CacheRequest.SetTreeScope", &e))?;
        Ok(request)
    }
}

fn cached(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Option<OwnedVariant> {
    // SAFETY: reads from the client-side cache of a live element owned by this thread.
    unsafe { el.GetCachedPropertyValue(id) }
        .ok()
        .map(OwnedVariant::new)
}

fn cached_bool(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> bool {
    cached(el, id).and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Reads every cached property. Individual failures degrade to defaults: a provider that
/// omits one property should not hide the element.
pub fn read_props(el: &IUIAutomationElement) -> UiProps {
    // SAFETY: cached getters read client-side data of a live element owned by this thread;
    // `GetRuntimeId` returns a SAFEARRAY that `take_i32_safearray` consumes exactly once.
    let mut props = unsafe {
        let text = |r: windows::core::Result<windows::core::BSTR>| {
            r.map(|b| b.to_string()).unwrap_or_default()
        };
        let control_type_id = el.CachedControlType().map(|t| t.0).unwrap_or(0);
        let bounds = el.CachedBoundingRectangle().ok().and_then(|r| {
            let rect = PhysicalRect::new(r.left, r.top, r.right, r.bottom);
            (!rect.is_empty()).then_some(rect)
        });
        UiProps {
            control_type_id,
            role: ControlRole::from_uia(control_type_id, cached_bool(el, UIA_IsDialogPropertyId)),
            name: text(el.CachedName()),
            automation_id: text(el.CachedAutomationId()),
            class_name: text(el.CachedClassName()),
            framework_id: text(el.CachedFrameworkId()),
            help_text: text(el.CachedHelpText()),
            process_id: el.CachedProcessId().map(|p| p as u32).unwrap_or(0),
            native_window_handle: el
                .CachedNativeWindowHandle()
                .ok()
                .filter(|h| !h.is_invalid())
                .map(|h| h.0 as usize as u64),
            runtime_id: el
                .GetRuntimeId()
                .map(|psa| take_i32_safearray(psa))
                .unwrap_or_default(),
            bounds,
            enabled: el.CachedIsEnabled().map(|b| b.as_bool()).unwrap_or(false),
            offscreen: el.CachedIsOffscreen().map(|b| b.as_bool()).unwrap_or(false),
            focused: el
                .CachedHasKeyboardFocus()
                .map(|b| b.as_bool())
                .unwrap_or(false),
            keyboard_focusable: el
                .CachedIsKeyboardFocusable()
                .map(|b| b.as_bool())
                .unwrap_or(false),
            is_password: el.CachedIsPassword().map(|b| b.as_bool()).unwrap_or(false),
            // Only labelled elements pay for the extra cross-process name read.
            labeled_by: el
                .CachedLabeledBy()
                .ok()
                .and_then(|label| label.CurrentName().ok())
                .map(|b| b.to_string())
                .filter(|s| !s.trim().is_empty()),
            ..UiProps::default()
        }
    };

    props.patterns = PATTERN_AVAILABILITY
        .iter()
        .filter(|(id, _)| cached_bool(el, *id))
        .map(|(_, p)| *p)
        .collect();

    if props.has_pattern(UiPattern::Value) {
        props.value_read_only = cached(el, UIA_ValueIsReadOnlyPropertyId).and_then(|v| v.as_bool());
    }
    if props.has_pattern(UiPattern::Toggle) {
        props.toggle_state = cached(el, UIA_ToggleToggleStatePropertyId)
            .and_then(|v| v.as_i32())
            .and_then(|s| match s {
                0 => Some(ToggleState::Off),
                1 => Some(ToggleState::On),
                2 => Some(ToggleState::Indeterminate),
                _ => None,
            });
    }
    if props.has_pattern(UiPattern::ExpandCollapse) {
        props.expand_state = cached(el, UIA_ExpandCollapseExpandCollapseStatePropertyId)
            .and_then(|v| v.as_i32())
            .and_then(|s| match s {
                0 => Some(ExpandState::Collapsed),
                1 => Some(ExpandState::Expanded),
                2 => Some(ExpandState::PartiallyExpanded),
                3 => Some(ExpandState::LeafNode),
                _ => None,
            });
    }
    if props.has_pattern(UiPattern::Scroll) {
        let percent = |id| cached(el, id).and_then(|v| v.as_f64()).unwrap_or(-1.0);
        props.scroll_percent = Some((
            percent(UIA_ScrollHorizontalScrollPercentPropertyId),
            percent(UIA_ScrollVerticalScrollPercentPropertyId),
        ));
    }
    if props.has_pattern(UiPattern::SelectionItem) {
        props.selected =
            cached(el, UIA_SelectionItemIsSelectedPropertyId).and_then(|v| v.as_bool());
    }
    props
}

/// Roles whose `Value` is short and meaningful in a snapshot. Documents are excluded: their
/// value is the whole text and belongs to an explicit read-text call.
fn wants_value(role: ControlRole) -> bool {
    matches!(
        role,
        ControlRole::Edit
            | ControlRole::ComboBox
            | ControlRole::Spinner
            | ControlRole::DataItem
            | ControlRole::Custom
    )
}

/// Fetches `ValuePattern.Value` live — never for sensitive fields, which are not read at all.
pub fn read_value(el: &IUIAutomationElement, props: &mut UiProps) {
    if !props.has_pattern(UiPattern::Value)
        || !wants_value(props.role)
        || winwright_security::is_sensitive(props)
    {
        return;
    }
    // SAFETY: live property read on an element owned by this thread.
    let value = unsafe { el.GetCurrentPropertyValue(UIA_ValueValuePropertyId) }
        .ok()
        .map(OwnedVariant::new)
        .and_then(|v| v.as_string());
    props.value = value.map(|v| v.chars().take(MAX_VALUE_CHARS).collect());
}

/// Roles that never need their children walked for a snapshot.
pub fn skip_children(role: ControlRole) -> bool {
    matches!(
        role,
        ControlRole::ScrollBar
            | ControlRole::Thumb
            | ControlRole::Separator
            | ControlRole::Image
            | ControlRole::ProgressBar
    )
}
