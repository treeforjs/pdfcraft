use super::*;
use kurbo::PathEl;

#[test]
fn pdf_default_pen_is_butt_miter_ten() {
    let list = compile(b"0 0 m 10 10 l S", || false).unwrap();
    let Paint::Stroke { style, .. } = &list.draws[0].paint else { panic!("stroke") };
    assert_eq!(style.width, 1.0);
    assert_eq!(style.miter_limit, 10.0);
    assert_eq!(style.join, Join::Miter);
    assert_eq!(style.start_cap, Cap::Butt);
    assert_eq!(style.end_cap, Cap::Butt);
}

#[test]
fn path_uses_the_ctm_and_q_does_not_restore_path() {
    let list = compile(b"q 2 0 0 3 10 20 cm 0 0 m q 5 6 l Q 4 0 0 4 0 0 cm S Q", || false);
    // cm inside a path object is not conforming and readers disagree: fall back.
    assert!(list.unwrap_err().0.contains("inside a path"));
    for content in ["0 0 m q 2 0 0 3 10 20 cm 5 6 l Q S", "q 2 0 0 2 0 0 cm 0 0 m Q 1 1 l S"] {
        assert!(compile(content.as_bytes(), || false).is_err(), "{content}");
    }
    let list = compile(b"q 2 0 0 3 10 20 cm 0 0 m q 5 6 l Q S Q", || false).unwrap();
    assert_eq!(list.draws[0].path.elements(), &[PathEl::MoveTo(Point::new(10.0, 20.0)), PathEl::LineTo(Point::new(20.0, 38.0))]);
    let Paint::Stroke { pen, .. } = &list.draws[0].paint else { panic!("stroke") };
    assert_eq!(*pen, Affine::new([2.0, 0.0, 0.0, 3.0, 10.0, 20.0]));
}

#[test]
fn a_segment_after_closepath_starts_a_new_subpath_at_the_closed_start() {
    let list = compile(b"0 0 m 10 0 l 10 10 l h 20 20 l 5 5 5 5 5 5 c S", || false).unwrap();
    assert_eq!(
        list.draws[0].path.elements(),
        &[
            PathEl::MoveTo(Point::ZERO),
            PathEl::LineTo(Point::new(10.0, 0.0)),
            PathEl::LineTo(Point::new(10.0, 10.0)),
            PathEl::ClosePath,
            PathEl::MoveTo(Point::ZERO),
            PathEl::LineTo(Point::new(20.0, 20.0)),
            PathEl::CurveTo(Point::new(5.0, 5.0), Point::new(5.0, 5.0), Point::new(5.0, 5.0)),
        ]
    );
}

#[test]
fn dash_phases_reduce_to_one_period() {
    for (content, offset) in
        [("[3 1] 9 d 0 0 m 1 0 l S", 1.0), ("[3] 7 d 0 0 m 1 0 l S", 1.0), ("[2 2] -1 d 0 0 m 1 0 l S", 3.0), ("[] 5 d 0 0 m 1 0 l S", 0.0)]
    {
        let list = compile(content.as_bytes(), || false).unwrap();
        let Paint::Stroke { style, .. } = &list.draws[0].paint else { panic!("stroke") };
        assert_eq!(style.dash_offset, offset, "{content}");
    }
}

#[test]
fn clipping_takes_effect_after_paint_and_q_restores_it() {
    let list = compile(b"q 0 0 10 10 re W* B 0 0 20 20 re f Q 0 0 30 30 re f", || false).unwrap();
    assert!(list.draws[0].clips.is_empty());
    assert!(list.draws[1].clips.is_empty());
    assert_eq!(list.draws[2].clips.len(), 1);
    assert_eq!(list.draws[2].clips[0].rule, FillRule::EvenOdd);
    assert!(list.draws[3].clips.is_empty());
}

#[test]
fn gray_rgb_and_dash_state_restore_independently() {
    let list = compile(b"0.5 g [3 0 2] 4 d q 1 0 0 rg 0 0 1 1 re f Q 0 0 2 2 re B", || false).unwrap();
    let Paint::Fill { rgb, .. } = list.draws[0].paint else { panic!("fill") };
    assert_eq!(rgb, [1.0, 0.0, 0.0]);
    let Paint::Fill { rgb, .. } = list.draws[1].paint else { panic!("fill") };
    assert_eq!(rgb, [0.5; 3]);
    let Paint::Stroke { rgb, style, .. } = &list.draws[2].paint else { panic!("stroke") };
    assert_eq!(*rgb, [0.0; 3]);
    assert_eq!(style.dash_pattern.as_slice(), &[3.0, 0.0, 2.0]);
    assert_eq!(style.dash_offset, 4.0);
}

#[test]
fn unsupported_late_operator_rejects_the_whole_list() {
    for suffix in ["BT", "/Im1 Do", "/GS1 gs", "0 0 0 1 k", "junk", "Q", "0 q", "1 2 m 0 l", "[0 0] 0 d"] {
        assert!(compile(format!("0 0 10 10 re f {suffix}").as_bytes(), || false).is_err(), "{suffix}");
    }
}

#[test]
fn cancellation_and_retained_clip_reference_budgets_are_atomic() {
    assert!(compile(b"0 0 1 1 re f", || true).is_err());
    let mut content = "0 0 10 10 re W n ".repeat(MAX_CLIPS);
    content.push_str(&"0 0 1 1 re f ".repeat(MAX_CLIP_REFERENCES / MAX_CLIPS + 1));
    let error = compile(content.as_bytes(), || false).unwrap_err();
    assert!(error.0.contains("clip references"), "{error}");
    assert!(compile(&vec![b' '; MAX_CONTENT + 1], || false).is_err());
}

#[test]
fn malformed_numeric_prefixes_and_long_tokens_are_not_repaired() {
    for text in ["1.2.3 0 m", "1e2 0 m", ". 0 m", "+ 0 m", "1--2 0 m", "[2.3.4] 0 d"] {
        assert!(compile(text.as_bytes(), || false).is_err(), "{text}");
    }
    let mut hostile = vec![b'.'];
    hostile.extend(std::iter::repeat_n(b'0', 500_000));
    hostile.extend(std::iter::repeat_n(b'a', 500_000));
    let mut reader = lexer::Reader::new(&hostile, &|| false);
    assert!(reader.next().unwrap_err().0.contains("token exceeds"));
    assert!(compile(b".5 +2. -3.25 4 0 0 cm 0 0 10 10 re f", || false).is_ok());
}

#[test]
fn large_comments_remain_cancellable_during_lexing() {
    use std::cell::Cell;
    let mut input = vec![b'%'];
    input.extend(std::iter::repeat_n(b'x', 900_000));
    let calls = Cell::new(0);
    assert!(
        compile(&input, || {
            calls.set(calls.get() + 1);
            calls.get() > 5
        })
        .unwrap_err()
        .0
        .contains("cancelled")
    );
    assert_eq!(calls.get(), 6);
}
