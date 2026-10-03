//! Real-desktop tests. Opt-in: `cargo test -p winwright-uia -- --ignored --test-threads=1`
//! on an unlocked interactive session. They only read the UI tree.

use std::time::Duration;

use tokio_util::sync::CancellationToken;
use winwright_contracts::backend::{
    InspectTarget, OperationContext, TreeRoot, UiAutomationBackend, UiTreeRequest,
};
use winwright_contracts::element::ControlRole;
use winwright_contracts::ids::SessionId;
use winwright_uia::UiaBackend;

fn ctx(timeout: Duration) -> OperationContext {
    OperationContext::new(
        SessionId::parse("live").unwrap(),
        timeout,
        CancellationToken::new(),
    )
}

#[tokio::test]
#[ignore = "needs an interactive desktop"]
async fn desktop_root_lists_top_level_windows() {
    winwright_win32::enable_per_monitor_dpi_awareness();
    let uia = UiaBackend::start().unwrap();
    let tree = uia
        .capture_tree(
            UiTreeRequest {
                root: TreeRoot::Desktop,
                max_depth: 2,
                max_nodes: 200,
                max_children: 100,
                include_offscreen: false,
            },
            &ctx(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    assert_eq!(
        tree.root.props.role,
        ControlRole::Pane,
        "desktop root is a pane"
    );
    assert!(!tree.root.children.is_empty());
    assert!(
        tree.root.children.iter().all(|c| c.children.is_empty()),
        "depth is bounded"
    );
    assert!(
        tree.truncated,
        "windows at the depth limit have children, so the tree says it was cut"
    );
    let slots: Vec<_> = std::iter::once(tree.root.key)
        .chain(tree.root.children.iter().map(|c| c.key))
        .collect();
    uia.release(slots).await.unwrap();
}

#[tokio::test]
#[ignore = "needs an interactive desktop"]
async fn focused_element_has_identity_and_ancestors() {
    winwright_win32::enable_per_monitor_dpi_awareness();
    let uia = UiaBackend::start().unwrap();
    let insp = uia
        .inspect(InspectTarget::Focused, &ctx(Duration::from_secs(10)))
        .await
        .unwrap();
    assert!(insp.props.process_id != 0);
    assert!(!insp.props.runtime_id.is_empty());
    assert!(insp.props.focused);
    // Re-inspecting through the stored key reaches the same element.
    let again = uia
        .inspect(
            InspectTarget::Element(insp.key),
            &ctx(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    assert_eq!(again.props.runtime_id, insp.props.runtime_id);
}

#[tokio::test]
#[ignore = "needs an interactive desktop"]
async fn cancelled_context_is_rejected_before_dispatch() {
    let uia = UiaBackend::start().unwrap();
    let c = ctx(Duration::from_secs(10));
    c.cancel.cancel();
    let err = uia.inspect(InspectTarget::Focused, &c).await.unwrap_err();
    assert_eq!(err.code().as_str(), "CANCELLED");
}
