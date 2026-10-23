//! Run-a-JavaScript actions in the app (egui_kittest): links, bookmarks and Page Open / Page
//! Close run their scripts; Link Properties, the bookmark menu and Page actions set them.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_engine::js::ActionSource;
use pdfcraft_engine::{BookmarkAction, LinkAction, PageTrigger};
use pdfcraft_ui_egui::{BmAction, Dialog, LinkKind, PdfCraftApp, ScriptTarget};

/// Two pages: a text field, a link that fills it in, a bookmark whose script does too, and a
/// Page Open script on page 2.
fn doc() -> Vec<u8> {
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R /Outlines 9 0 R /AcroForm << /Fields [5 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 6 0 R >> >> >> >>",
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 300 300] >>",
        "<< /Type /Page /Parent 2 0 R /Annots [5 0 R 7 0 R] >>",
        "<< /Type /Page /Parent 2 0 R /AA << /O 8 0 R >> >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (greeting) /Rect [20 250 280 270] /P 3 0 R /F 4 >>",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        "<< /Type /Annot /Subtype /Link /Rect [20 100 120 130] /P 3 0 R /A << /S /JavaScript /JS (getField\\('greeting'\\).value = 'from the link';) >> >>",
        "<< /S /JavaScript /JS (getField\\('greeting'\\).value = 'page ' + \\(this.pageNum + 1\\) + ' opened';) >>",
        "<< /Type /Outlines /First 10 0 R /Last 10 0 R /Count 1 >>",
        "<< /Title (Say hello) /Parent 9 0 R /A << /S /JavaScript /JS (getField\\('greeting'\\).value = 'from the bookmark';) >> >>",
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn harness() -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("actions.pdf", None, doc()).unwrap();
        app
    });
    h.run_steps(4);
    h
}

fn greeting(h: &Harness<'static, PdfCraftApp>) -> Vec<String> {
    let app = h.state();
    let id = app.active_ids().unwrap().1;
    app.session.get(id).unwrap().form.iter().find(|f| f.name == "greeting").unwrap().value.clone()
}

#[test]
fn links_bookmarks_and_pages_run_their_scripts() {
    let mut h = harness();
    let id = h.state().active_ids().unwrap().1;
    assert_eq!(h.state().session.get(id).unwrap().info.page_actions, [1]);
    // A link's script (what a click on it runs).
    let link = h.state().session.get(id).unwrap().links[0].index;
    h.state_mut().run_action_script(id, ActionSource::Link { page: 0, index: link });
    h.run_steps(2);
    assert_eq!(greeting(&h), ["from the link"]);
    // A bookmark without a page runs its script when clicked.
    h.state_mut().bookmark_action(BmAction::Run(vec![0]));
    h.run_steps(2);
    assert_eq!(greeting(&h), ["from the bookmark"]);
    // Going to page 2 runs its Page Open script once.
    let i = h.state().active_ids().unwrap().0;
    h.state_mut().views[i].go_to_page(1);
    h.run_steps(3);
    assert_eq!(greeting(&h), ["page 2 opened"]);
    h.state_mut().bookmark_action(BmAction::Run(vec![0]));
    h.run_steps(3);
    assert_eq!(greeting(&h), ["from the bookmark"], "staying on the page doesn't run Page Open again");
    // With JavaScript off, nothing runs.
    h.state_mut().session.set_javascript(false);
    h.state_mut().views[i].go_to_page(0);
    h.run_steps(2);
    h.state_mut().views[i].go_to_page(1);
    h.run_steps(2);
    assert_eq!(greeting(&h), ["from the bookmark"]);
}

#[test]
fn a_link_script_dialog_waits_for_the_user() {
    let mut h = harness();
    let id = h.state().active_ids().unwrap().1;
    h.state_mut().apply_edit(pdfcraft_engine::Edit::SetLink {
        page: 0,
        index: 1,
        rect: None,
        action: Some(LinkAction::JavaScript("if (app.alert('Fill it in?', 2, 2) == 4) getField('greeting').value = 'yes';".into())),
        style: None,
    });
    h.state_mut().run_action_script(id, ActionSource::Link { page: 0, index: 1 });
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::JsQuestion));
    assert!(greeting(&h).iter().all(String::is_empty), "nothing applied while it waits");
    h.get_by_label("Yes").click();
    h.run_steps(3);
    assert_eq!(greeting(&h), ["yes"]);
}

#[test]
fn link_properties_bookmarks_and_page_actions_set_scripts() {
    let mut h = harness();
    let id = h.state().active_ids().unwrap().1;
    // Link Properties ▸ Run a JavaScript.
    let l = h.state().session.get(id).unwrap().links[0].clone();
    h.state_mut().link_draft = Some(pdfcraft_ui_egui::LinkDraft {
        page: 0,
        rect: l.rect,
        index: Some(l.index),
        kind: LinkKind::JavaScript,
        url: "https://".into(),
        script: "app.alert('changed')".into(),
        target: 1,
        view: Default::default(),
        dest_name: String::new(),
        names: Vec::new(),
        here: None,
        style: l.style,
        keep: None,
    });
    h.state_mut().dialog = Some(Dialog::LinkProps);
    h.run_steps(2);
    h.get_by_label("Run a JavaScript");
    h.get_by_label("OK").click();
    h.run_steps(3);
    assert_eq!(h.state().session.get(id).unwrap().links[0].action, LinkAction::JavaScript("app.alert('changed')".into()));

    // Bookmark ▸ Run a JavaScript…
    h.state_mut().bookmark_action(BmAction::EditScript(vec![0]));
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::ActionScript));
    h.get_by_label("Bookmark Action");
    h.state_mut().action_script.as_mut().unwrap().scripts[0] = "console.println('bm')".into();
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(3);
    assert_eq!(h.state().session.bookmark_action(id, &[0]).unwrap(), BookmarkAction::JavaScript("console.println('bm')".into()));

    // Page actions… for page 1: a Page Close script, saved as one undo step.
    assert!(h.state_mut().execute("tools.page_actions"));
    h.run_steps(2);
    assert_eq!(h.state().action_script.as_ref().unwrap().target, ScriptTarget::Page(0));
    h.get_by_label("Page Actions: page 1");
    h.get_by_label("Page Close").click();
    h.run_steps(2);
    {
        let d = h.state_mut().action_script.as_mut().unwrap();
        assert_eq!(d.trigger, PageTrigger::Close);
        d.scripts[1] = "console.println('bye')".into();
    }
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None);
    let doc = h.state().session.get(id).unwrap();
    assert_eq!(doc.page_action(0, PageTrigger::Close).as_deref(), Some("console.println('bye')"));
    assert_eq!(doc.can_undo(), Some("Set page action"));
    // Removing page 2's Page Open script.
    h.state_mut().open_page_actions(1);
    h.run_steps(2);
    assert_eq!(h.state().action_script.as_ref().unwrap().scripts[0], "getField('greeting').value = 'page ' + (this.pageNum + 1) + ' opened';");
    h.get_by_label("Remove").click();
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(3);
    assert_eq!(h.state().session.get(id).unwrap().page_action(1, PageTrigger::Open), None);
    assert!(h.state().session.get(id).unwrap().info.page_actions.contains(&0));
}
