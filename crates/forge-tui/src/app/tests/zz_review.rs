//! TEMPORARY review harness — dumps the startup frame as text. Removed after use.

use super::helpers::{focus_test_app, render_app_text};

#[tokio::test]
async fn zz_review_startup_frame() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files.visible = true;
    app.runtime.provider = "OpenCode".into();
    app.runtime.model_label = "opencode-go/deepseek-v4.1-flash".into();
    app.session_view.loaded_skills_count = 27;
    for (w, h) in [(120u16, 40u16), (170u16, 40u16), (250u16, 50u16)] {
        let text = render_app_text(&mut app, w, h);
        println!("===== {w}x{h} =====\n{text}===== end {w}x{h} =====\n");
    }
}
