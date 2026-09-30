//! Control-pattern execution (spec §11). Runs only on the UIA worker thread.

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Accessibility::*;
use windows::core::{BSTR, Interface};
use winwright_contracts::backend::{ScrollAmount, UiPatternAction, UiProps};
use winwright_contracts::geometry::PhysicalPoint;
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::com::OwnedVariant;

fn hr(code: u32) -> i32 {
    code as i32
}

/// Maps a failed pattern call to a typed error. A provider timeout after dispatch means the
/// operation may have run, so it is reported as an unknown outcome and never retried.
pub fn action_error(operation: &str, target: &str, err: &windows::core::Error) -> WinwrightError {
    let code = err.code().0;
    if code == hr(UIA_E_ELEMENTNOTAVAILABLE) {
        WinwrightError::ElementStale {
            reference: target.to_owned(),
            reason: "the element no longer exists".into(),
        }
    } else if code == hr(UIA_E_ELEMENTNOTENABLED) {
        WinwrightError::invalid(format!("{target} is disabled"))
    } else if code == hr(UIA_E_NOTSUPPORTED) || code == hr(UIA_E_INVALIDOPERATION) {
        WinwrightError::UnsupportedPattern {
            element: target.to_owned(),
            pattern: operation.to_owned(),
        }
    } else if code == hr(UIA_E_TIMEOUT) {
        WinwrightError::ActionOutcomeUnknown {
            operation: operation.to_owned(),
            reason: format!("{target} did not answer in time (it may be showing a modal dialog)"),
        }
    } else {
        WinwrightError::Platform {
            operation: operation.to_owned(),
            hresult: code,
        }
    }
}

fn pattern<T: Interface>(
    el: &IUIAutomationElement,
    id: UIA_PATTERN_ID,
    name: &str,
    target: &str,
) -> WinwrightResult<T> {
    // SAFETY: live element owned by the worker thread; the returned interface stays on it.
    unsafe { el.GetCurrentPatternAs::<T>(id) }.map_err(|e| {
        if e.code().0 == hr(UIA_E_ELEMENTNOTAVAILABLE) {
            action_error(name, target, &e)
        } else {
            WinwrightError::UnsupportedPattern {
                element: target.to_owned(),
                pattern: name.to_owned(),
            }
        }
    })
}

fn amount(a: ScrollAmount) -> windows::Win32::UI::Accessibility::ScrollAmount {
    match a {
        ScrollAmount::LargeDecrement => ScrollAmount_LargeDecrement,
        ScrollAmount::SmallDecrement => ScrollAmount_SmallDecrement,
        ScrollAmount::NoAmount => ScrollAmount_NoAmount,
        ScrollAmount::LargeIncrement => ScrollAmount_LargeIncrement,
        ScrollAmount::SmallIncrement => ScrollAmount_SmallIncrement,
    }
}

pub struct PatternOutput {
    pub text: Option<(String, &'static str)>,
    pub point: Option<PhysicalPoint>,
}

/// Executes one pattern operation. `props` are the element's fresh properties, used for
/// labels in errors and to refuse reading sensitive fields.
pub fn execute(
    el: &IUIAutomationElement,
    props: &UiProps,
    action: &UiPatternAction,
) -> WinwrightResult<PatternOutput> {
    let target = props.label();
    let t = target.as_str();
    let mut out = PatternOutput {
        text: None,
        point: None,
    };
    // SAFETY: every call in this block is a COM call on interfaces owned by the worker
    // thread, and BSTR arguments outlive the calls.
    unsafe {
        match action {
            UiPatternAction::Invoke => {
                pattern::<IUIAutomationInvokePattern>(el, UIA_InvokePatternId, "Invoke", t)?
                    .Invoke()
                    .map_err(|e| action_error("Invoke", t, &e))?
            }
            UiPatternAction::Select => pattern::<IUIAutomationSelectionItemPattern>(
                el,
                UIA_SelectionItemPatternId,
                "SelectionItem",
                t,
            )?
            .Select()
            .map_err(|e| action_error("SelectionItem.Select", t, &e))?,
            UiPatternAction::Toggle => {
                pattern::<IUIAutomationTogglePattern>(el, UIA_TogglePatternId, "Toggle", t)?
                    .Toggle()
                    .map_err(|e| action_error("Toggle", t, &e))?
            }
            UiPatternAction::Expand => pattern::<IUIAutomationExpandCollapsePattern>(
                el,
                UIA_ExpandCollapsePatternId,
                "ExpandCollapse",
                t,
            )?
            .Expand()
            .map_err(|e| action_error("ExpandCollapse.Expand", t, &e))?,
            UiPatternAction::Collapse => pattern::<IUIAutomationExpandCollapsePattern>(
                el,
                UIA_ExpandCollapsePatternId,
                "ExpandCollapse",
                t,
            )?
            .Collapse()
            .map_err(|e| action_error("ExpandCollapse.Collapse", t, &e))?,
            UiPatternAction::SetValue(value) => {
                let bstr = BSTR::from(value.as_str());
                pattern::<IUIAutomationValuePattern>(el, UIA_ValuePatternId, "Value", t)?
                    .SetValue(&bstr)
                    .map_err(|e| action_error("Value.SetValue", t, &e))?
            }
            UiPatternAction::ScrollIntoView => pattern::<IUIAutomationScrollItemPattern>(
                el,
                UIA_ScrollItemPatternId,
                "ScrollItem",
                t,
            )?
            .ScrollIntoView()
            .map_err(|e| action_error("ScrollItem.ScrollIntoView", t, &e))?,
            UiPatternAction::Scroll {
                horizontal,
                vertical,
            } => pattern::<IUIAutomationScrollPattern>(el, UIA_ScrollPatternId, "Scroll", t)?
                .Scroll(amount(*horizontal), amount(*vertical))
                .map_err(|e| action_error("Scroll", t, &e))?,
            UiPatternAction::SetFocus => {
                el.SetFocus().map_err(|e| action_error("SetFocus", t, &e))?
            }
            UiPatternAction::GetText { max_chars } => {
                if winwright_security::is_sensitive(props) {
                    return Err(WinwrightError::SensitiveField { element: target });
                }
                out.text = Some(read_text(el, *max_chars, t)?);
            }
            UiPatternAction::ClickablePoint => {
                let mut p = POINT::default();
                if el.GetClickablePoint(&mut p).is_ok_and(|got| got.as_bool()) {
                    out.point = Some(PhysicalPoint { x: p.x, y: p.y });
                }
            }
        }
    }
    Ok(out)
}

fn clamp_chars(s: String, max_chars: u32) -> String {
    if s.chars().count() <= max_chars as usize {
        s
    } else {
        s.chars().take(max_chars as usize).collect()
    }
}

/// TextPattern, then ValuePattern, then Name (spec §11 read_text order).
fn read_text(
    el: &IUIAutomationElement,
    max_chars: u32,
    target: &str,
) -> WinwrightResult<(String, &'static str)> {
    let max = max_chars.clamp(1, 1_000_000);
    // SAFETY: COM calls on interfaces owned by the worker thread.
    unsafe {
        if let Ok(text) = el.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
            && let Ok(range) = text.DocumentRange()
            && let Ok(s) = range.GetText(max as i32)
        {
            return Ok((clamp_chars(s.to_string(), max), "TextPattern"));
        }
        if let Some(v) = el
            .GetCurrentPropertyValue(UIA_ValueValuePropertyId)
            .ok()
            .map(OwnedVariant::new)
            .and_then(|v| v.as_string())
        {
            return Ok((clamp_chars(v, max), "ValuePattern"));
        }
        let name = el
            .CurrentName()
            .map_err(|e| action_error("Name", target, &e))?
            .to_string();
        Ok((clamp_chars(name, max), "Name"))
    }
}
