//! Printing tools: list printers, and print (or save the print-ready PDF) with Acrobat's Print
//! dialog options.

use pdfcraft_engine::print::{
    self, Binding, BookletSubset, Content, Layout, Margins, Orientation, PAPERS, PageOrder, PageSetup, SizeMode, Subset, spool,
};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed, write_atomic};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

impl Automation {
    pub(crate) fn printers(&self) -> Result<Value> {
        let list: Vec<Value> = spool::printers().into_iter().map(|p| json!({ "name": p.name, "default": p.default })).collect();
        Ok(json!({ "count": list.len(), "printers": list }))
    }

    pub(crate) fn printer_options(&self, a: &Args) -> Result<Value> {
        let printer = a.str("printer")?;
        let list: Vec<Value> = spool::printer_options(printer)
            .into_iter()
            .map(|o| {
                let choices: Vec<Value> = o.choices.iter().map(|(value, label)| json!({ "value": value, "label": label })).collect();
                json!({ "key": o.key, "label": o.label, "group": o.group, "default": o.default, "choices": choices })
            })
            .collect();
        Ok(json!({ "printer": printer, "count": list.len(), "options": list }))
    }

    pub(crate) fn doc_print(&mut self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let (id, count) = (doc.id, doc.info.pages.len());
        let labels: Vec<String> = doc.info.pages.iter().map(|p| p.label.clone()).collect();
        let subset = match a.opt_str("subset")?.unwrap_or("all") {
            "all" => Subset::All,
            "odd" => Subset::Odd,
            "even" => Subset::Even,
            s => return Err(bad(format!("unknown subset {s:?}"))),
        };
        let pages = print::select_pages(count, a.opt_str("pages")?, &labels, subset, a.opt_bool("reverse")?.unwrap_or(false))
            .map_err(|e| bad(e.to_string()))?;
        let layout = match a.opt_str("layout")?.unwrap_or("fit") {
            "fit" => Layout::Size(SizeMode::Fit),
            "actual" => Layout::Size(SizeMode::Actual),
            "shrink" => Layout::Size(SizeMode::Shrink),
            "custom" => Layout::Size(SizeMode::Custom(a.opt_num("scale")?.ok_or_else(|| bad("custom needs scale (percent)"))?)),
            "multiple" => {
                let per = a.opt_int("per_sheet")?.unwrap_or(2).clamp(1, 256) as usize;
                let order = match a.opt_str("order")?.unwrap_or("horizontal") {
                    "horizontal" => PageOrder::Horizontal,
                    "horizontal-reversed" => PageOrder::HorizontalReversed,
                    "vertical" => PageOrder::Vertical,
                    "vertical-reversed" => PageOrder::VerticalReversed,
                    "cut-stack" => PageOrder::CutStack,
                    o => return Err(bad(format!("unknown order {o:?}"))),
                };
                match Layout::multiple(per) {
                    Layout::Multiple { cols, rows, .. } => Layout::Multiple {
                        cols,
                        rows,
                        order,
                        border: a.opt_bool("border")?.unwrap_or(false),
                        auto_rotate: a.opt_bool("auto_rotate")?.unwrap_or(true),
                    },
                    l => l,
                }
            }
            "booklet" => Layout::Booklet {
                subset: match a.opt_str("booklet_subset")?.unwrap_or("both") {
                    "both" => BookletSubset::BothSides,
                    "front" => BookletSubset::FrontOnly,
                    "back" => BookletSubset::BackOnly,
                    s => return Err(bad(format!("unknown booklet_subset {s:?}"))),
                },
                binding: if a.opt_str("binding")? == Some("right") { Binding::Right } else { Binding::Left },
            },
            "poster" => Layout::Poster {
                scale: a.opt_num("scale")?.unwrap_or(200.0),
                overlap: a.opt_num("overlap")?.unwrap_or(18.0),
                cut_marks: a.opt_bool("cut_marks")?.unwrap_or(true),
            },
            l => return Err(bad(format!("unknown layout {l:?}"))),
        };
        if matches!(layout, Layout::Multiple { order: PageOrder::CutStack, .. }) && a.opt_str("duplex")?.unwrap_or("off") != "off" {
            return Err(bad("cut-and-stack printing needs duplex off (single-sided sheets)"));
        }
        if a.opt_str("order")? == Some("cut-stack") && !matches!(layout, Layout::Multiple { .. }) {
            return Err(bad("cut-stack order needs layout multiple"));
        }
        // Page Setup gives the paper, orientation and margins unless this job chooses its own.
        let setup = page_setup_args(a, *self.session.page_setup())?;
        let content = match a.opt_str("comments_forms")?.unwrap_or("document-and-markups") {
            "document" => Content::Document,
            "document-and-markups" => Content::DocumentAndMarkups,
            "document-and-stamps" => Content::DocumentAndStamps,
            "form-fields-only" => Content::FormFieldsOnly,
            c => return Err(bad(format!("unknown comments_forms {c:?}"))),
        };
        let settings = print::Settings { pages, layout, content, ..Default::default() }.with_page_setup(&setup);
        // Advanced ▸ Print As Image: each sheet goes to the printer as one picture.
        let as_image = match (a.opt_bool("as_image")?.unwrap_or(false), a.opt_num("dpi")?) {
            (true, dpi) => {
                let dpi = dpi.unwrap_or(PRINT_AS_IMAGE_DPI);
                if !(dpi.is_finite() && (MIN_PRINT_DPI..=MAX_PRINT_DPI).contains(&dpi)) {
                    return Err(bad(format!("dpi must be between {MIN_PRINT_DPI} and {MAX_PRINT_DPI}, not {dpi}")));
                }
                Some(dpi)
            }
            (false, Some(_)) => return Err(bad("dpi goes with as_image true (print as image)")),
            (false, None) => None,
        };
        let sizes: Vec<(f64, f64)> = self.doc(a)?.info.pages.iter().map(|p| (p.width as f64, p.height as f64)).collect();
        let sheets = print::layout(&sizes, &settings).map_err(|e| bad(e.to_string()))?.len();
        // Document Will Print runs first (what it changes is printed); Did Print once it's out.
        self.session.run_document_action(id, pdfcraft_engine::DocTrigger::WillPrint).map_err(failed)?;
        let (bytes, rasters) = match as_image {
            Some(dpi) => {
                let (bytes, r) = self.session.print_pdf_as_image(id, &settings, dpi).map_err(failed)?;
                (bytes, Some((dpi, r)))
            }
            None => (self.session.print_pdf(id, &settings).map_err(failed)?, None),
        };
        let mut out = json!({ "sheets": sheets, "pages": settings.pages.len(), "bytes": bytes.len(), "page_setup": page_setup_json(&setup) });
        if let Some((dpi, r)) = rasters {
            out["as_image"] = json!({
                "dpi": dpi,
                "sheets": r.iter().map(|p| json!({ "sheet": p.page + 1, "width": p.width, "height": p.height, "dpi": p.resolution, "images": p.images })).collect::<Vec<_>>(),
            });
        }
        match (a.opt_str("path")?, a.opt_str("printer")?) {
            (Some(p), None) => {
                let path = self.resolve(p, true)?;
                write_atomic(&path, &bytes)?;
                out["path"] = json!(path.to_string_lossy());
            }
            (None, Some(printer)) => {
                let name = self.doc(a)?.name.clone();
                let job = spool::Job {
                    printer: if printer == "default" { None } else { Some(printer.to_string()) },
                    copies: a.opt_int("copies")?.unwrap_or(1).clamp(1, 999) as u32,
                    collate: a.opt_bool("collate")?.unwrap_or(true),
                    duplex: match a.opt_str("duplex")?.unwrap_or("off") {
                        "off" => spool::Duplex::Off,
                        "long-edge" => spool::Duplex::LongEdge,
                        "short-edge" => spool::Duplex::ShortEdge,
                        d => return Err(bad(format!("unknown duplex {d:?}"))),
                    },
                    grayscale: a.opt_bool("grayscale")?.unwrap_or(false),
                    title: name,
                    options: match a.get("options") {
                        None => Vec::new(),
                        Some(Value::Object(m)) => m
                            .iter()
                            .map(|(k, v)| match v.as_str() {
                                Some(v) => Ok((k.clone(), v.to_string())),
                                None => Err(bad(format!("options.{k} must be a string (a choice from printer_options)"))),
                            })
                            .collect::<Result<Vec<_>>>()?,
                        Some(_) => return Err(bad("options must be an object of option keys and choices from printer_options")),
                    },
                };
                out["job"] = json!(spool::submit(&bytes, &job).map_err(|e| failed(e.to_string()))?);
            }
            _ => return Err(bad("pass either path (save the print-ready PDF) or printer (a name from printers, or \"default\")")),
        }
        self.session.run_document_action(id, pdfcraft_engine::DocTrigger::DidPrint).map_err(failed)?;
        Ok(out)
    }

    /// Output Preview's Total Area Coverage check (read-only).
    pub(crate) fn total_area_coverage(&self, a: &Args) -> Result<Value> {
        use pdfcraft_engine::preflight::CoverageOptions;
        let doc = self.doc(a)?;
        let defaults = CoverageOptions::default();
        let max_regions = match a.opt_int("max_regions")? {
            Some(n) => usize::try_from(n).map_err(|_| bad("max_regions must be between 1 and 50"))?,
            None => defaults.max_regions,
        };
        let options = CoverageOptions {
            limit: a.opt_num("limit")?.unwrap_or(defaults.limit),
            dpi: a.opt_num("dpi")?.unwrap_or(defaults.dpi),
            max_regions,
            overprint: a.opt_bool("overprint")?.unwrap_or(defaults.overprint),
        };
        options.validate().map_err(bad)?;
        let count = doc.info.pages.len();
        let (pages, pages_truncated) = if a.get("pages").is_some() {
            let pages = self.pages(a, "pages")?;
            if pages.len() > MAX_COVERAGE_PAGES {
                return Err(bad(format!("pages may list at most {MAX_COVERAGE_PAGES} pages per call")));
            }
            (pages, false)
        } else {
            ((0..count.min(MAX_COVERAGE_PAGES)).collect(), count > MAX_COVERAGE_PAGES)
        };
        let results = doc.ink_coverage(&pages, &options).map_err(failed)?;
        let rect = |page: usize, r: [f64; 4]| -> Value {
            let r32 = r.map(|v| v as f32);
            json!(doc.info.pages.get(page).map_or(r32, |p| crate::comments::rect_to_view(p, r32)))
        };
        let point = |page: usize, p: [f64; 2]| -> Value { crate::separations::view_point(doc.info.pages.get(page), p) };
        let list: Vec<Value> = results
            .iter()
            .map(|c| {
                json!({
                    "page": c.page + 1, "max": c.max, "flagged": c.flagged(), "over_area": c.over_area, "over_percent": c.over_percent,
                    "region_count": c.region_count,
                    "max_at": c.max_at.map(|p| point(c.page, p)), "pdf_max_at": c.max_at,
                    "within_limit": c.max <= options.limit,
                    "regions": c.regions.iter().map(|r| json!({ "rect": rect(c.page, r.bbox), "pdf_rect": r.bbox, "max": r.max, "area": r.area })).collect::<Vec<_>>(),
                    "dpi": c.dpi, "exact_paints": c.exact_paints, "converted_paints": c.converted_paints,
                    "unmeasured": c.unmeasured, "approximated": c.approximated, "truncated": c.truncated,
                })
            })
            .collect();
        let flagged: Vec<usize> = results.iter().filter(|c| c.flagged()).map(|c| c.page + 1).collect();
        Ok(json!({
            "doc": doc.id.0, "limit": options.limit, "flagged_pages": flagged, "pages": list, "pages_truncated": pages_truncated,
            "max": results.iter().map(|c| c.max).fold(0.0, f64::max),
            "overprint": options.overprint,
            "conversion": results.first().map(|c| c.conversion.clone()),
            "method": pdfcraft_engine::pdfa::coverage::METHOD,
        }))
    }
}

impl Automation {
    /// Output Preview's overprint, rich-black and registration-colour warnings.
    pub(crate) fn colour_warnings(&self, a: &Args) -> Result<Value> {
        use pdfcraft_engine::pdfa::colour_warnings::{MAX_FINDINGS, NOT_CHECKED};
        use pdfcraft_engine::preflight::ColourWarningOptions;
        let doc = self.doc(a)?;
        let d = ColourWarningOptions::default();
        let pages = if a.get("pages").is_some() { self.pages(a, "pages")? } else { Vec::new() };
        let max_findings = match a.opt_int("max_findings")? {
            None => d.max_findings,
            Some(n) => usize::try_from(n)
                .ok()
                .filter(|n| (1..=MAX_FINDINGS).contains(n))
                .ok_or_else(|| bad(format!("max_findings must be 1 to {MAX_FINDINGS}, not {n}")))?,
        };
        let options = ColourWarningOptions {
            pages,
            rich_black_k: a.opt_num("rich_black_k")?.unwrap_or(d.rich_black_k),
            rich_black_cmy: a.opt_num("rich_black_cmy_total")?.unwrap_or(d.rich_black_cmy),
            small_text_pt: a.opt_num("small_text_pt")?.unwrap_or(d.small_text_pt),
            max_findings,
        };
        options.validate().map_err(bad)?;
        let r = doc.colour_warnings(&options).map_err(failed)?;
        let rect = |page: usize, r: [f64; 4]| -> Value {
            let r32 = r.map(|v| v as f32);
            json!(doc.info.pages.get(page).map_or(r32, |p| crate::comments::rect_to_view(p, r32)))
        };
        let findings: Vec<Value> = r
            .findings
            .iter()
            .map(|f| {
                json!({
                    "kind": f.kind.id(),
                    "severity": f.severity.id(),
                    "page": f.page + 1,
                    "object": f.object.map(|o| o.num),
                    "message": f.message,
                    "paint": f.paint.map(|p| p.id()),
                    "colour": f.colour.as_ref().map(|c| json!({
                        "space": c.space, "components": c.components, "cmyk": c.cmyk, "total": c.total(), "via_alternate": c.via_alternate,
                    })),
                    "text_size": f.text_size,
                    "overprint_mode": f.overprint_mode,
                    "rect": f.bbox.map(|b| rect(f.page, b)),
                    "pdf_rect": f.bbox,
                    "occurrences": f.occurrences,
                })
            })
            .collect();
        Ok(json!({
            "doc": doc.id.0,
            "pages_checked": r.pages.iter().map(|p| p + 1).collect::<Vec<_>>(),
            "thresholds": { "rich_black_k": options.rich_black_k, "rich_black_cmy_total": options.rich_black_cmy, "small_text_pt": options.small_text_pt },
            "warnings": r.warnings(),
            "counts": r.counts,
            "occurrences": r.occurrences,
            "findings": findings,
            "findings_truncated": r.findings_truncated,
            "unresolved": r.unresolved,
            "complete": r.limits.is_empty(),
            "limits": r.limits,
            "not_checked": NOT_CHECKED,
        }))
    }
}

/// `doc_print`'s print-as-image resolution: the default and the accepted range (the
/// rasterizer's, shared with flatten transparency).
const PRINT_AS_IMAGE_DPI: f64 = 300.0;
const MIN_PRINT_DPI: f64 = pdfcraft_engine::preflight::RASTER_MIN_RESOLUTION;
const MAX_PRINT_DPI: f64 = pdfcraft_engine::preflight::RASTER_MAX_RESOLUTION;

/// The mark names `print_add_printer_marks` takes.
const MARK_NAMES: [&str; 5] = ["trim", "bleed", "registration", "color_bars", "page_info"];

impl Automation {
    /// Print Production ▸ Add Printer Marks.
    pub(crate) fn add_printer_marks(&mut self, a: &Args) -> Result<Value> {
        use pdfcraft_engine::{Edit, MarkStyle, PrinterMarks};
        let doc = self.doc(a)?;
        let id = doc.id;
        let pages = match a.get("pages") {
            Some(_) => self.pages(a, "pages")?,
            None => (0..doc.info.pages.len()).collect(),
        };
        let d = PrinterMarks::default();
        let mut settings = PrinterMarks { file_name: doc.name.clone(), ..d.clone() };
        if a.get("marks").is_some() {
            let names = a.strs("marks")?;
            if names.is_empty() {
                return Err(bad(format!("marks must list at least one of {}", MARK_NAMES.join(", "))));
            }
            if let Some(n) = names.iter().find(|n| **n != "all" && !MARK_NAMES.contains(n)) {
                return Err(bad(format!("unknown mark {n:?} (all, {})", MARK_NAMES.join(", "))));
            }
            let all = names.contains(&"all");
            let has = |m: &str| all || names.contains(&m);
            settings.trim = has("trim");
            settings.bleed = has("bleed");
            settings.registration = has("registration");
            settings.color_bars = has("color_bars");
            settings.page_info = has("page_info");
        }
        settings.line_weight = a.opt_num("line_weight")?.unwrap_or(d.line_weight);
        settings.offset = a.opt_num("offset")?.unwrap_or(d.offset);
        if let Some(s) = a.opt_str("style")? {
            settings.style = MarkStyle::from_name(s).ok_or_else(|| bad(format!("unknown style {s:?} (western, eastern)")))?;
        }
        if let Some(f) = a.opt_str("file_name")? {
            settings.file_name = f.to_string();
        }
        settings.validate().map_err(|e| bad(e.to_string()))?;
        let plan = self.session.printer_marks_preview(id, &pages, &settings).map_err(failed)?;
        let mut out = self.apply(a, Edit::AddPrinterMarks { pages, settings })?;
        out["marked"] = json!(
            plan.iter()
                .map(|p| json!({ "page": p.page + 1, "trim": p.trim, "bleed": p.bleed, "media": p.media, "margin": p.margin }))
                .collect::<Vec<_>>()
        );
        Ok(out)
    }
}

/// Pages `print_total_area_coverage` measures per call.
const MAX_COVERAGE_PAGES: usize = 200;

/// The object type names `print_convert_colors` takes.
const COLOR_OBJECTS: [&str; 3] = ["text", "vector", "images"];

impl Automation {
    /// Print Production ▸ Convert Colors.
    pub(crate) fn convert_colors(&mut self, a: &Args) -> Result<Value> {
        use pdfcraft_engine::{ColorConversion, ColorDestination, ColorObjects, ColorSources, RenderingIntent};
        let doc = self.doc(a)?;
        let id = doc.id;
        let pages = match a.get("pages") {
            Some(_) => self.pages(a, "pages")?,
            None => (0..doc.info.pages.len()).collect(),
        };
        let mut settings = ColorConversion::default();
        let profile = match a.opt_str("profile")? {
            Some(p) => {
                let path = self.resolve(p, false)?;
                let max = pdfcraft_engine::color_profile_limit();
                let size = std::fs::metadata(&path).map_err(|e| failed(format!("{}: {e}", path.display())))?.len();
                if size > max as u64 {
                    return Err(bad(format!("profile is larger than {} MB", max >> 20)));
                }
                Some(std::fs::read(&path).map_err(|e| failed(format!("{}: {e}", path.display())))?)
            }
            None => None,
        };
        settings.destination = match (a.opt_str("destination")?, profile) {
            (None | Some("ICCBased"), Some(bytes)) => ColorDestination::Icc(std::sync::Arc::new(bytes)),
            (Some("ICCBased"), None) => return Err(bad("destination ICCBased needs profile (an ICC profile file)")),
            (Some(_), Some(_)) => return Err(bad("profile goes with destination ICCBased only")),
            (None | Some("DeviceCMYK"), None) => ColorDestination::DeviceCmyk,
            (Some("DeviceGray"), None) => ColorDestination::DeviceGray,
            (Some("DeviceRGB"), None) => ColorDestination::DeviceRgb,
            (Some(d), None) => return Err(bad(format!("unknown destination {d:?} (DeviceCMYK, DeviceGray, DeviceRGB, ICCBased)"))),
        };
        if a.get("sources").is_some() {
            let names = a.strs("sources")?;
            if names.is_empty() {
                return Err(bad(format!("sources must list at least one of all, {}", ColorSources::NAMES.join(", "))));
            }
            let mut sources = ColorSources::NONE;
            for n in names {
                if n == "all" {
                    sources = ColorSources::ALL;
                } else if !sources.set(n) {
                    return Err(bad(format!("unknown source space {n:?} (all, {})", ColorSources::NAMES.join(", "))));
                }
            }
            settings.sources = sources;
        }
        if a.get("objects").is_some() {
            let names = a.strs("objects")?;
            if names.is_empty() {
                return Err(bad(format!("objects must list at least one of all, {}", COLOR_OBJECTS.join(", "))));
            }
            if let Some(n) = names.iter().find(|n| **n != "all" && !COLOR_OBJECTS.contains(n)) {
                return Err(bad(format!("unknown object type {n:?} (all, {})", COLOR_OBJECTS.join(", "))));
            }
            let all = names.contains(&"all");
            let has = |m: &str| all || names.contains(&m);
            settings.objects = ColorObjects { text: has("text"), vector: has("vector"), images: has("images") };
        }
        // The colour settings give the default intent, and a device destination converts
        // through the working space of its family when that isn't the factory one.
        let colour = self.session.color_management();
        settings.intent = colour.settings().intent;
        if let Some(i) = a.opt_str("intent")? {
            settings.intent =
                RenderingIntent::from_name(i).ok_or_else(|| bad(format!("unknown intent {i:?} ({})", RenderingIntent::NAMES.join(", "))))?;
        }
        let family = match settings.destination {
            ColorDestination::DeviceCmyk => Some(pdfcraft_engine::color_settings::ProfileFamily::Cmyk),
            ColorDestination::DeviceGray => Some(pdfcraft_engine::color_settings::ProfileFamily::Gray),
            ColorDestination::DeviceRgb => Some(pdfcraft_engine::color_settings::ProfileFamily::Rgb),
            ColorDestination::Icc(_) => None,
        };
        let mut through = None;
        if a.opt_bool("use_working_space")?.unwrap_or(true)
            && let Some((bytes, label)) = family.and_then(|f| colour.working_destination(f))
        {
            settings.destination = ColorDestination::Icc(bytes);
            settings.device_output = true;
            through = Some(label);
        }
        settings.preserve_black = a.opt_bool("preserve_black")?.unwrap_or(true);
        settings.convert_spots = match a.opt_str("spots")?.unwrap_or("preserve") {
            "preserve" => false,
            "convert" => true,
            s => return Err(bad(format!("unknown spots {s:?} (preserve, convert)"))),
        };
        settings.output_intent = a.opt_bool("output_intent")?.unwrap_or(false);
        settings.output_condition = a.opt_str("output_condition")?.map(str::to_string);
        settings.validate().map_err(|e| bad(e.to_string()))?;
        let sources = settings.sources.names();
        let destination = if settings.device_output {
            match family {
                Some(pdfcraft_engine::color_settings::ProfileFamily::Gray) => "DeviceGray",
                Some(pdfcraft_engine::color_settings::ProfileFamily::Rgb) => "DeviceRGB",
                _ => "DeviceCMYK",
            }
        } else {
            settings.destination.name()
        };
        let intent = settings.intent.name();
        let report = self.session.convert_colors(id, pages, settings).map_err(failed)?;
        let mut out = crate::summary(self.doc(a)?);
        out["changed"] = json!(report.changed());
        out["destination"] = json!(destination);
        out["through"] = json!(through);
        out["intent"] = json!(intent);
        out["sources"] = json!(sources);
        out["pages_changed"] = json!(report.pages.iter().map(|p| p + 1).collect::<Vec<_>>());
        out["converted"] = json!({
            "colors": report.colors,
            "images": report.images,
            "inline_images": report.inline_images,
            "shadings": report.shadings,
            "forms": report.forms,
            "patterns": report.patterns,
            "annotations": report.annotations,
        });
        out["approximate"] = json!(report.approximate);
        out["output_intent"] = json!(report.output_intent);
        out["skipped"] = json!(report.skipped);
        Ok(out)
    }
}

/// The page setup with the `paper`, `paper_size`, `orientation` and `margins` arguments applied to
/// `base`, checked.
fn page_setup_args(a: &Args, base: PageSetup) -> Result<PageSetup> {
    let paper = match (a.opt_str("paper")?, a.nums::<2>("paper_size")?) {
        (Some(_), Some(_)) => return Err(bad("pass paper (a preset name) or paper_size ([width, height] in points), not both")),
        (Some(p), None) => print::paper_preset(p)
            .ok_or_else(|| bad(format!("unknown paper {p:?} ({}); or pass paper_size [width, height] in points", paper_names())))?,
        (None, Some([w, h])) => (w, h),
        (None, None) => base.paper,
    };
    let orientation = match a.opt_str("orientation")? {
        None => base.orientation,
        Some("auto") => Orientation::Auto,
        Some("portrait") => Orientation::Portrait,
        Some("landscape") => Orientation::Landscape,
        Some(o) => return Err(bad(format!("unknown orientation {o:?} (auto, portrait, landscape)"))),
    };
    let margins = match a.get("margins") {
        None => base.margins,
        Some(Value::Number(n)) => Margins::uniform(n.as_f64().ok_or_else(|| bad("margins must be a number of points"))?),
        Some(Value::Object(m)) => {
            let mut out = base.margins;
            for (k, v) in m {
                let v = v.as_f64().filter(|v| v.is_finite()).ok_or_else(|| bad(format!("margins.{k} must be a number of points")))?;
                match k.as_str() {
                    "top" => out.top = v,
                    "right" => out.right = v,
                    "bottom" => out.bottom = v,
                    "left" => out.left = v,
                    _ => return Err(bad(format!("unknown margin {k:?} (top, right, bottom, left)"))),
                }
            }
            out
        }
        Some(_) => return Err(bad("margins must be a number (every side) or an object with top, right, bottom and left, in points")),
    };
    PageSetup::new(paper, orientation, margins).map_err(|e| bad(e.to_string()))
}

fn paper_names() -> String {
    PAPERS.iter().map(|p| p.0).collect::<Vec<_>>().join(", ")
}

fn orientation_id(o: Orientation) -> &'static str {
    match o {
        Orientation::Auto => "auto",
        Orientation::Portrait => "portrait",
        Orientation::Landscape => "landscape",
    }
}

fn page_setup_json(s: &PageSetup) -> Value {
    let m = s.margins;
    let (w, h) = s.paper;
    json!({
        "paper": { "name": s.paper_name(), "width": w, "height": h, "width_in": w / 72.0, "height_in": h / 72.0, "width_mm": w / 72.0 * 25.4, "height_mm": h / 72.0 * 25.4 },
        "orientation": orientation_id(s.orientation),
        "margins": { "top": m.top, "right": m.right, "bottom": m.bottom, "left": m.left },
        // The room inside the margins, either way the sheet is turned.
        "printable": {
            "portrait": [w - m.left - m.right, h - m.top - m.bottom],
            "landscape": [h - m.left - m.right, w - m.top - m.bottom],
        },
        "default": *s == PageSetup::default(),
    })
}

impl Automation {
    /// File ▸ Page Setup: read the session's paper, orientation and margins.
    pub(crate) fn page_setup_get(&self) -> Result<Value> {
        let mut out = page_setup_json(self.session.page_setup());
        out["papers"] = json!(PAPERS.iter().map(|(n, (w, h))| json!({ "name": n, "width": w, "height": h })).collect::<Vec<_>>());
        out["limits"] = json!({ "min_paper": print::MIN_PAPER, "max_paper": print::MAX_PAPER, "min_printable": print::MIN_PRINTABLE });
        Ok(out)
    }

    /// File ▸ Page Setup: change them for this session.
    pub(crate) fn page_setup_set(&mut self, a: &Args) -> Result<Value> {
        let base = if a.opt_bool("reset")?.unwrap_or(false) { PageSetup::default() } else { *self.session.page_setup() };
        let setup = page_setup_args(a, base)?;
        self.session.set_page_setup(setup).map_err(|e| bad(e.to_string()))?;
        self.page_setup_get()
    }
}
