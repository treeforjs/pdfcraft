//! Recognition drafts: inspect and correct words before a single explicit document edit.

use serde_json::{Value, json};

use pdfcraft_engine::DocId;
use pdfcraft_engine::ocr::OcrReview;

use crate::{Args, Automation, Result, ToolError, failed};

/// Drafts held at once, and the recognized words and text bytes they may hold together
/// (each draft is also limited to one million words and 64 MiB by the engine).
const MAX_DRAFTS: usize = 8;
const MAX_DRAFT_WORDS: usize = 2_000_000;
const MAX_DRAFT_TEXT_BYTES: usize = 128 * 1024 * 1024;

/// The text direction corrected for recognition on a page (`null` when auto-rotate and deskew
/// were off): `rotation` 0/90/180/270 degrees counterclockwise, `skew` degrees counterclockwise.
pub(crate) fn orientation_json(page: &pdfcraft_engine::ocr::OcrPage) -> Value {
    page.orientation
        .map_or(Value::Null, |o| json!({"page": page.page + 1, "rotation": o.rotation, "skew": (f64::from(o.skew) * 10.0).round() / 10.0}))
}

/// What despeckling the recognition copy removed (`null` when despeckle was off).
pub(crate) fn despeckle_json(stats: Option<&pdfcraft_engine::ocr::DespeckleStats>) -> Value {
    stats.map_or(
        Value::Null,
        |s| json!({"dark_specks": s.dark_specks, "light_specks": s.light_specks, "impulses": s.impulses, "pixels_changed": s.pixels_changed}),
    )
}

/// What background removal found and changed on the recognition copy (`null` when it was off).
pub(crate) fn background_json(stats: Option<&pdfcraft_engine::ocr::BackgroundStats>) -> Value {
    stats.map_or(
        Value::Null,
        |s| json!({"background": s.background, "white_point": s.white_point, "pixels_whitened": s.pixels_whitened, "pixels_changed": s.pixels_changed}),
    )
}

/// Editable Text and Images on a page (`null` for the searchable outputs): the line count,
/// how many lines use each font, each line's text, font, size (points), colour and horizontal
/// scaling, and the words kept as image with invisible text (1-based word, text, the backend's
/// confidence or `null`, and why).
pub(crate) fn editable_json(page: &pdfcraft_engine::ocr::OcrPage) -> Value {
    let Some(layout) = &page.editable else { return Value::Null };
    let round = |v: f64| (v * 100.0).round() / 100.0;
    let hex = |c: [u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
    let lines: Vec<Value> = layout
        .lines
        .iter()
        .map(|l| {
            let text =
                l.words.iter().filter_map(|w| page.words.get(w.index)).map(|w| w.text.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" ");
            let words = l.words.iter().filter_map(|w| Some((page.words.get(w.index)?.text.as_str(), w.width))).filter(|(t, _)| !t.trim().is_empty());
            let scale = pdfcraft_engine::ocr::horizontal_scale(l.font, words, l.size);
            json!({
                "text": text, "font": l.font.base_font(), "family": l.font.family.name(), "bold": l.font.bold,
                "size": round(l.size), "color": hex(l.color), "paper": hex(l.paper), "horizontal_scale": scale.map(round),
                "words": l.words.iter().map(|w| w.index + 1).collect::<Vec<_>>(),
            })
        })
        .collect();
    let low: Vec<Value> = layout
        .suspects
        .iter()
        .map(|s| {
            json!({
                "word": s.word + 1, "text": page.words.get(s.word).map(|w| w.text.as_str()), "confidence": s.confidence,
                "reason": s.reason.name(),
            })
        })
        .collect();
    json!({ "lines": layout.lines.len(), "fonts": layout.fonts(), "line_styles": lines, "low_confidence": low })
}

fn review_json(review: &OcrReview) -> Value {
    let pages: Vec<_> = review
        .pages()
        .iter()
        .map(|page| {
            let words: Vec<_> = page
                .words
                .iter()
                .enumerate()
                .map(|(index, word)| {
                    json!({
                        "word": index + 1, "text": word.text, "origin": word.origin, "across": word.across, "up": word.up
                    })
                })
                .collect();
            json!({"page": page.page + 1, "text": page.text(), "words": words, "skipped": page.skipped, "orientation": orientation_json(page), "despeckle": despeckle_json(page.despeckle.as_ref()), "background": background_json(page.background.as_ref()), "editable": editable_json(page)})
        })
        .collect();
    json!({"doc": review.document().0, "generation": review.generation(), "words": review.words(), "pages": pages})
}

impl Automation {
    pub(crate) fn ocr_prepare(&mut self, args: &Args) -> Result<Value> {
        let settings = self.ocr_settings(args)?;
        let pages = if args.opt_ints("pages")?.is_some() { self.pages(args, "pages")? } else { Vec::new() };
        let id = self.doc(args)?.id;
        if self.ocr_reviews.len() >= MAX_DRAFTS && !self.ocr_reviews.contains_key(&id) {
            return Err(failed("at most eight recognition drafts may be open; apply or cancel a draft first"));
        }
        // Copy and modification authority are checked here, before the models load or any
        // page is rendered.
        let job = self.session.ocr_job_checked(id, &pages, settings).map_err(failed)?;
        let recognizer = pdfcraft_engine::ocr::engine().map_err(failed)?;
        let review = job.run(recognizer.as_ref(), |_, _| true).map_err(failed)?;
        let others = self.ocr_reviews.iter().filter(|(doc, _)| **doc != id).map(|(_, r)| r);
        let (words, bytes) =
            others.fold((review.word_slots(), review.text_bytes()), |(w, b), r| (w.saturating_add(r.word_slots()), b.saturating_add(r.text_bytes())));
        if words > MAX_DRAFT_WORDS || bytes > MAX_DRAFT_TEXT_BYTES {
            return Err(failed("open recognition drafts would exceed two million words or 128 MiB of text; apply or cancel other drafts first"));
        }
        let reply = review_json(&review);
        self.ocr_reviews.insert(id, review);
        Ok(reply)
    }

    /// The draft for `id`, revalidated against the document as it is now (open, copy and
    /// modification authority, same revision). A draft that fails can never become valid
    /// again, so it is discarded and its words are not returned.
    fn current_ocr_draft(&mut self, id: DocId) -> Result<&mut OcrReview> {
        let review = self.ocr_reviews.get(&id).ok_or_else(|| failed("no recognition draft; call ocr_prepare first"))?;
        if let Err(error) = self.session.validate_ocr_review(review) {
            self.ocr_reviews.remove(&id);
            return Err(failed(format!("{error}; the recognition draft was discarded")));
        }
        self.ocr_reviews.get_mut(&id).ok_or_else(|| failed("no recognition draft; call ocr_prepare first"))
    }

    pub(crate) fn ocr_review(&mut self, args: &Args) -> Result<Value> {
        let id = self.doc(args)?.id;
        Ok(review_json(self.current_ocr_draft(id)?))
    }

    pub(crate) fn ocr_correct(&mut self, args: &Args) -> Result<Value> {
        let id = self.doc(args)?.id;
        let index = |name: &str| -> Result<usize> {
            args.int(name)?
                .checked_sub(1)
                .and_then(|v| usize::try_from(v).ok())
                .ok_or_else(|| ToolError::InvalidArgs(format!("{name} must be a positive index from ocr_review")))
        };
        let (page, word, text) = (index("page")?, index("word")?, args.str("text")?);
        let review = self.current_ocr_draft(id)?;
        review.correct(page, word, text).map_err(ToolError::InvalidArgs)?;
        Ok(review_json(review))
    }

    pub(crate) fn ocr_apply(&mut self, args: &Args) -> Result<Value> {
        let id = self.doc(args)?.id;
        self.current_ocr_draft(id)?;
        let review = self.ocr_reviews.remove(&id).ok_or_else(|| failed("no recognition draft; call ocr_prepare first"))?;
        let words = self.session.apply_ocr_review(&review).map_err(|e| failed(format!("{e}; the recognition draft was discarded")))?;
        Ok(json!({"doc": id.0, "words": words, "applied": true}))
    }

    pub(crate) fn ocr_cancel(&mut self, args: &Args) -> Result<Value> {
        let id = self.doc(args)?.id;
        let canceled = self.ocr_reviews.remove(&id).is_some();
        Ok(json!({"doc": id.0, "canceled": canceled}))
    }
}
