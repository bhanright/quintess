//! Form scripts: the JavaScript that real-world forms (tax forms, invoices,
//! applications) attach to their fields to total columns, check entries,
//! show numbers as currency and dates in a pattern, and filter what can be
//! typed.
//!
//! Each field may carry four scripts in its additional-actions dictionary
//! (`/AA`): keystroke (`K`), validate (`V`), calculate (`C`) and format
//! (`F`). The form's calculation order is `/AcroForm /CO`, and functions
//! the scripts share live in the document-level scripts (the `/Names
//! /JavaScript` tree). [`apply_field_value`] runs them the way Acrobat does
//! when someone finishes typing into a field: keystroke and validation
//! (either may turn the value down), then every calculation in order, then
//! formatting. The PDF stores the plain value in `/V` ("1234.5") and the
//! appearance shows the formatted text ("$1,234.50").
//!
//! # Safety
//!
//! Scripts run in Boa, a JavaScript engine written in Rust, which has no
//! file, network or process objects to begin with; the Acrobat functions
//! that would reach outside (mail, submit, launch, print) do nothing.
//! Only the four field scripts and the document-level scripts they need
//! are ever run: never link actions, page open or close actions or the
//! document's `/OpenAction`, which is where hostile PDFs put code meant
//! to run on opening. Every batch of scripts runs on its own thread under
//! limits on loop iterations, recursion and time, so a form that loops
//! forever is stopped instead of hanging Quintess.
//!
//! The Acrobat object model (event, Field, app, util and the AForm
//! built-ins) is JavaScript in `form_scripts_prelude.js`.

use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::sync::mpsc;
use std::task::Poll;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use boa_engine::{Context, Script, Source};
use lopdf::{Dictionary, Object, ObjectId};
use serde::{Deserialize, Serialize};

use crate::document::Pdf;
use crate::forms::{self, FieldInfo, FieldType};
use crate::strings::{object_to_text, text_string};

/// The Acrobat object model, loaded before any form script runs.
const PRELUDE: &str = include_str!("form_scripts_prelude.js");

/// How long one batch of scripts (everything one change sets off) may run.
const SCRIPT_TIME_LIMIT: Duration = Duration::from_secs(2);

/// How much longer to wait for the script thread to notice the time limit
/// before giving up on it. A script stuck inside a built-in that calls back
/// into JavaScript (`Array.prototype.forEach`, say) only notices when it
/// returns, so the thread is left to finish on its own.
const SCRIPT_THREAD_GRACE: Duration = Duration::from_secs(2);

/// Iterations one function's loops may run in total. Real form scripts
/// loop over a few hundred fields at most.
const LOOP_ITERATION_LIMIT: u64 = 1_000_000;

/// How deeply script functions may call one another.
const RECURSION_LIMIT: usize = 400;

/// How much work (in Boa's instruction costs) runs between checks of the
/// time limit: often enough to stop promptly, rarely enough to cost nothing.
const INSTRUCTIONS_BETWEEN_TIME_CHECKS: u32 = 20_000;

/// The script thread's stack. Built-ins that call back into scripts nest
/// on the native stack, so give them room.
const SCRIPT_THREAD_STACK_BYTES: usize = 16 * 1024 * 1024;

/// Scripts longer than this are not read (no real form has one).
const LONGEST_SCRIPT_BYTES: usize = 4 * 1024 * 1024;

/// The scripts on one field, as JavaScript source.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct FieldScripts {
    /// Runs on every keystroke and once more when the entry is committed.
    pub keystroke: Option<String>,
    /// Checks a committed value; may turn it down.
    pub validate: Option<String>,
    /// Works the field's value out from other fields.
    pub calculate: Option<String>,
    /// Turns the stored value into the text shown.
    pub format: Option<String>,
}

impl FieldScripts {
    pub fn is_empty(&self) -> bool {
        self.keystroke.is_none() && self.validate.is_none() && self.calculate.is_none() && self.format.is_none()
    }
}

/// A field that has at least one script.
#[derive(Clone, Debug, Serialize)]
pub struct ScriptedFieldSummary {
    pub name: String,
    #[serde(flatten)]
    pub scripts: FieldScripts,
}

/// A document-level script: shared functions the field scripts call.
#[derive(Clone, Debug, Serialize)]
pub struct DocumentScript {
    pub name: String,
    pub source: String,
}

/// Every script in a form, for inspection.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ScriptInventory {
    pub fields: Vec<ScriptedFieldSummary>,
    pub document_scripts: Vec<DocumentScript>,
    /// Fields with calculations, in the order they run.
    pub calculation_order: Vec<String>,
    /// Scripts Quintess never runs, by where they are (the document's open
    /// action, page actions, links and buttons).
    pub never_run: Vec<String>,
}

/// What running a form's scripts did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ScriptOutcome {
    /// Fields whose stored value or shown text was rewritten, in form order.
    pub changed_fields: Vec<String>,
    /// Set when a keystroke or validation script turned the new value
    /// down; the field then keeps its old value.
    pub rejected: Option<RejectedValue>,
    /// Messages scripts showed with `app.alert`, in order.
    pub alerts: Vec<String>,
    /// Scripts that failed or were stopped, as "field (calculate): reason".
    pub script_errors: Vec<String>,
}

/// A value a form's scripts would not accept.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RejectedValue {
    pub field: String,
    pub value: String,
    /// The script's own message, or a general one when it gave none.
    pub message: String,
}

/// What a keystroke script made of one keystroke.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct KeystrokeOutcome {
    pub allowed: bool,
    /// The text to insert, which a script may have changed (a mask adds
    /// its dashes and parentheses).
    pub change: String,
    pub alerts: Vec<String>,
}

/// Lists every script in a form, the order calculations run in, and the
/// scripts Quintess leaves alone.
pub fn list_scripts(pdf: &Pdf) -> ScriptInventory {
    let form = FormSnapshot::read(pdf);
    ScriptInventory {
        fields: form.fields.iter().filter(|field| !field.scripts.is_empty()).map(|field| ScriptedFieldSummary { name: field.info.name.clone(), scripts: field.scripts.clone() }).collect(),
        calculation_order: form.calculation_order.iter().map(|&index| form.fields[index].info.name.clone()).collect(),
        document_scripts: form.document_scripts,
        never_run: scripts_never_run(pdf),
    }
}

/// Whether any field in the form has a script.
pub fn has_scripts(pdf: &Pdf) -> bool {
    FormSnapshot::read(pdf).has_field_scripts()
}

/// Sets a field as if someone had typed `new_value` and moved on: runs its
/// keystroke and validation scripts (either may refuse the value, which
/// then is not stored), stores the value, runs every calculation in the
/// form's calculation order, and formats every field that changed.
/// Checkboxes take their on-state name or "Off" (or yes/no, as
/// [`forms::fill_fields`] does), radio groups the button to select.
pub fn apply_field_value(pdf: &mut Pdf, field_name: &str, new_value: &str) -> Result<ScriptOutcome> {
    let form = FormSnapshot::read(pdf);
    let Some(target) = form.fields.iter().position(|field| field.info.name == field_name) else { bail!("no field named \"{field_name}\"") };
    if matches!(form.fields[target].info.field_type, FieldType::PushButton | FieldType::Signature | FieldType::Unknown) {
        bail!("the field \"{field_name}\" does not take a value");
    }
    if !form.has_field_scripts() {
        forms::fill_fields(pdf, &BTreeMap::from([(field_name.to_string(), new_value.to_string())]))?;
        let changed = forms::field_value(pdf, field_name)? != form.fields[target].info.value;
        return Ok(ScriptOutcome { changed_fields: if changed { vec![field_name.to_string()] } else { Vec::new() }, ..Default::default() });
    }
    // Checkbox values arrive as yes/no; scripts compare export values.
    let value = form.fields[target].stored_form_of(new_value);
    match run_scripts(form.clone(), Request::Apply { target, value: value.clone() }) {
        Ok(report) => {
            let mut outcome = ScriptOutcome { alerts: report.alerts.clone(), script_errors: report.errors.clone(), ..Default::default() };
            if let Some(rejected) = report.rejected.clone() {
                outcome.rejected = Some(rejected);
                return Ok(outcome);
            }
            apply_report(pdf, &form, &report, &mut outcome)?;
            Ok(outcome)
        }
        Err(problem) => {
            // The scripts could not run at all; store the value as typed so
            // the person's entry isn't lost.
            forms::fill_fields(pdf, &BTreeMap::from([(field_name.to_string(), new_value.to_string())]))?;
            Ok(ScriptOutcome { changed_fields: vec![field_name.to_string()], script_errors: vec![problem.to_string()], ..Default::default() })
        }
    }
}

/// Runs every calculation in order and formats every field with a format
/// script, without changing any entry: after importing data or filling a
/// copy in a mail merge.
pub fn recalculate(pdf: &mut Pdf) -> Result<ScriptOutcome> {
    let form = FormSnapshot::read(pdf);
    if form.calculation_order.is_empty() && !form.fields.iter().any(|field| field.scripts.format.is_some()) {
        return Ok(ScriptOutcome::default());
    }
    match run_scripts(form.clone(), Request::Recalculate) {
        Ok(report) => {
            let mut outcome = ScriptOutcome { alerts: report.alerts.clone(), script_errors: report.errors.clone(), ..Default::default() };
            apply_report(pdf, &form, &report, &mut outcome)?;
            Ok(outcome)
        }
        Err(problem) => Ok(ScriptOutcome { script_errors: vec![problem.to_string()], ..Default::default() }),
    }
}

/// Fills fields by name as [`forms::fill_fields`] does, then recalculates
/// so totals and formatted text match the new values. As with data
/// imported into Acrobat, the values are stored without keystroke or
/// validation checks. Returns the names that matched no field.
pub fn fill_fields_and_calculate(pdf: &mut Pdf, values: &BTreeMap<String, String>) -> Result<(Vec<String>, ScriptOutcome)> {
    let unmatched = forms::fill_fields(pdf, values)?;
    let mut outcome = recalculate(pdf)?;
    // The filled fields changed too, whether or not a script touched them.
    let fields = forms::list_fields(pdf);
    outcome.changed_fields = fields.into_iter().map(|field| field.name).filter(|name| values.contains_key(name) || outcome.changed_fields.contains(name)).collect();
    Ok((unmatched, outcome))
}

/// Runs a field's keystroke script for one keystroke: `change` replaces the
/// characters from `selection_start` to `selection_end` of `current` (both
/// counted in UTF-16 units, as JavaScript counts; `None` means the end of
/// the text). Fields without a keystroke script allow everything.
pub fn keystroke(pdf: &Pdf, field_name: &str, current: &str, change: &str, selection: Option<(usize, usize)>) -> Result<KeystrokeOutcome> {
    let form = FormSnapshot::read(pdf);
    let Some(target) = form.fields.iter().position(|field| field.info.name == field_name) else { bail!("no field named \"{field_name}\"") };
    if form.fields[target].scripts.keystroke.is_none() {
        return Ok(KeystrokeOutcome { allowed: true, change: change.to_string(), alerts: Vec::new() });
    }
    let end_of_text = current.encode_utf16().count();
    let (selection_start, selection_end) = selection.unwrap_or((end_of_text, end_of_text));
    let report = run_scripts(form, Request::Keystroke { target, current: current.to_string(), change: change.to_string(), selection_start, selection_end })?;
    let (allowed, change) = report.keystroke.unwrap_or((true, change.to_string()));
    Ok(KeystrokeOutcome { allowed, change, alerts: report.alerts })
}

/// Whether the field's keystroke script lets `change` be typed at the end
/// of `current`. A script that fails allows the keystroke.
pub fn keystroke_allows(pdf: &Pdf, field_name: &str, current: &str, change: &str) -> bool {
    keystroke(pdf, field_name, current, change, None).map(|outcome| outcome.allowed).unwrap_or(true)
}

// ---------------------------------------------------------------------------
// Reading the form.

/// A field and everything its scripts may ask about it.
#[derive(Clone, Debug)]
struct ScriptedField {
    info: FieldInfo,
    scripts: FieldScripts,
    /// List and combo box choices as (shown text, export value).
    items: Vec<(String, String)>,
    /// Each checkbox or radio button's on-state, in widget order.
    export_values: Vec<String>,
    /// Acrobat's display constant: 0 visible, 1 hidden, 2 no print, 3 no view.
    display: u8,
    default_value: String,
    editable: bool,
}

impl ScriptedField {
    /// The value as it will be stored: checkboxes turn yes/no into their
    /// on-state name or "Off", as forms::fill_fields does.
    fn stored_form_of(&self, value: &str) -> String {
        if self.info.field_type != FieldType::CheckBox || self.export_values.iter().any(|on| on == value) {
            return value.to_string();
        }
        let wanted_on = matches!(value.to_ascii_lowercase().as_str(), "true" | "yes" | "on" | "1" | "x" | "checked");
        if wanted_on { self.export_values.first().cloned().unwrap_or_else(|| "Yes".into()) } else { "Off".into() }
    }
}

/// The form as the script thread sees it: plain data that can be sent to
/// another thread.
#[derive(Clone, Debug, Default)]
struct FormSnapshot {
    fields: Vec<ScriptedField>,
    document_scripts: Vec<DocumentScript>,
    /// Indexes into `fields` of those with calculations, in running order.
    calculation_order: Vec<usize>,
    page_count: u32,
}

impl FormSnapshot {
    fn read(pdf: &Pdf) -> FormSnapshot {
        let fields: Vec<ScriptedField> = forms::list_fields(pdf).into_iter().map(|info| read_field(pdf, info)).collect();
        let calculation_order = calculation_order(pdf, &fields);
        FormSnapshot { document_scripts: document_scripts(pdf), calculation_order, page_count: pdf.page_count() as u32, fields }
    }

    fn has_field_scripts(&self) -> bool {
        self.fields.iter().any(|field| !field.scripts.is_empty())
    }
}

fn read_field(pdf: &Pdf, info: FieldInfo) -> ScriptedField {
    let Some(field_id) = info.id else {
        return ScriptedField { info, scripts: FieldScripts::default(), items: Vec::new(), export_values: Vec::new(), display: 0, default_value: String::new(), editable: false };
    };
    let widgets = forms::widget_ids(pdf, field_id);
    let scripts = FieldScripts {
        keystroke: trigger_script(pdf, field_id, &widgets, b"K"),
        validate: trigger_script(pdf, field_id, &widgets, b"V"),
        calculate: trigger_script(pdf, field_id, &widgets, b"C"),
        format: trigger_script(pdf, field_id, &widgets, b"F"),
    };
    let field = pdf.doc.get_dictionary(field_id).cloned().unwrap_or_default();
    // /Opt entries are either the text itself or [export value, shown text].
    let items = forms::inherited(pdf, &field, b"Opt")
        .and_then(|options| options.as_array().ok())
        .map(|options| {
            options
                .iter()
                .filter_map(|option| match pdf.resolve(option) {
                    Object::Array(pair) => {
                        let export = pair.first().map(|o| pdf.resolve(o)).and_then(object_to_text)?;
                        let shown = pair.get(1).map(|o| pdf.resolve(o)).and_then(object_to_text).unwrap_or_else(|| export.clone());
                        Some((shown, export))
                    }
                    other => object_to_text(other).map(|text| (text.clone(), text)),
                })
                .collect()
        })
        .unwrap_or_default();
    let export_values = if matches!(info.field_type, FieldType::CheckBox | FieldType::RadioGroup) { widgets.iter().filter_map(|&widget| forms::on_state_name(pdf, widget)).collect() } else { Vec::new() };
    let widget_flags = widgets.first().and_then(|&widget| pdf.doc.get_dictionary(widget).ok()).and_then(|widget| widget.get(b"F").ok()).and_then(|flags| flags.as_i64().ok()).unwrap_or(4);
    let field_flags = forms::inherited(pdf, &field, b"Ff").and_then(|flags| flags.as_i64().ok()).unwrap_or(0);
    ScriptedField {
        display: display_from_annotation_flags(widget_flags),
        default_value: forms::inherited(pdf, &field, b"DV").and_then(object_to_text).unwrap_or_default(),
        editable: field_flags & FIELD_FLAG_EDITABLE_COMBO != 0,
        info,
        scripts,
        items,
        export_values,
    }
}

const FIELD_FLAG_READ_ONLY: i64 = 1;
const FIELD_FLAG_REQUIRED: i64 = 1 << 1;
const FIELD_FLAG_EDITABLE_COMBO: i64 = 1 << 18;
const ANNOTATION_FLAG_HIDDEN: i64 = 1 << 1;
const ANNOTATION_FLAG_PRINT: i64 = 1 << 2;
const ANNOTATION_FLAG_NO_VIEW: i64 = 1 << 5;

/// Acrobat's display constant for a widget's annotation flags.
fn display_from_annotation_flags(flags: i64) -> u8 {
    if flags & ANNOTATION_FLAG_HIDDEN != 0 {
        1
    } else if flags & ANNOTATION_FLAG_NO_VIEW != 0 {
        3
    } else if flags & ANNOTATION_FLAG_PRINT == 0 {
        2
    } else {
        0
    }
}

/// Annotation flags for a display constant, keeping unrelated flags.
fn annotation_flags_for_display(flags: i64, display: u8) -> i64 {
    let cleared = flags & !(ANNOTATION_FLAG_HIDDEN | ANNOTATION_FLAG_NO_VIEW | ANNOTATION_FLAG_PRINT);
    match display {
        1 => cleared | ANNOTATION_FLAG_HIDDEN | (flags & ANNOTATION_FLAG_PRINT),
        2 => cleared,
        3 => cleared | ANNOTATION_FLAG_NO_VIEW | ANNOTATION_FLAG_PRINT,
        _ => cleared | ANNOTATION_FLAG_PRINT,
    }
}

/// The JavaScript a field runs for one trigger (K, V, C or F). Usually on
/// the field; some producers put it on the field's widget instead.
fn trigger_script(pdf: &Pdf, field_id: ObjectId, widgets: &[ObjectId], trigger: &[u8]) -> Option<String> {
    std::iter::once(field_id).chain(widgets.iter().copied()).find_map(|holder| {
        let holder = pdf.doc.get_dictionary(holder).ok()?;
        let actions = pdf.resolve(holder.get(b"AA").ok()?).as_dict().ok()?;
        let action = actions.get(trigger).ok()?;
        let source = javascript_of_action(pdf, action);
        (!source.trim().is_empty()).then_some(source)
    })
}

/// The JavaScript of an action and the actions chained after it (`/Next`).
fn javascript_of_action(pdf: &Pdf, action: &Object) -> String {
    let mut source = String::new();
    let mut visited = HashSet::new();
    collect_action_javascript(pdf, action, &mut visited, &mut source, 0);
    source
}

fn collect_action_javascript(pdf: &Pdf, action: &Object, visited: &mut HashSet<ObjectId>, source: &mut String, depth: usize) {
    if depth > 32 {
        return;
    }
    if let Object::Reference(id) = action {
        if !visited.insert(*id) {
            return;
        }
    }
    let Ok(dictionary) = pdf.resolve(action).as_dict() else { return };
    if dictionary.get(b"S").ok().and_then(|kind| kind.as_name().ok()) == Some(b"JavaScript") {
        if let Some(text) = dictionary.get(b"JS").ok().and_then(|code| script_text(pdf, code)) {
            if !source.is_empty() {
                source.push('\n');
            }
            source.push_str(&text);
        }
    }
    match dictionary.get(b"Next").map(|next| pdf.resolve(next)) {
        Ok(Object::Array(next_actions)) => {
            for next in next_actions {
                collect_action_javascript(pdf, next, visited, source, depth + 1);
            }
        }
        Ok(_) => collect_action_javascript(pdf, dictionary.get(b"Next").expect("checked above"), visited, source, depth + 1),
        Err(_) => {}
    }
}

/// A script's text: a string, or a stream holding it.
fn script_text(pdf: &Pdf, code: &Object) -> Option<String> {
    match pdf.resolve(code) {
        Object::Stream(stream) => {
            let bytes = stream.get_plain_content_with_limit(LONGEST_SCRIPT_BYTES).ok()?;
            // Stream text is encoded as text strings are (UTF-16 with a byte
            // order mark, or PDFDocEncoding).
            object_to_text(&Object::String(bytes, lopdf::StringFormat::Literal))
        }
        other => object_to_text(other),
    }
}

/// Document-level scripts, in name order (the order Acrobat runs them).
fn document_scripts(pdf: &Pdf) -> Vec<DocumentScript> {
    let mut scripts = Vec::new();
    let Ok(catalog) = pdf.doc.catalog() else { return scripts };
    let Some(tree) = catalog.get(b"Names").ok().and_then(|names| pdf.resolve(names).as_dict().ok()).and_then(|names| names.get(b"JavaScript").ok()) else { return scripts };
    let mut visited = HashSet::new();
    collect_name_tree(pdf, tree, &mut visited, &mut scripts, 0);
    scripts
}

fn collect_name_tree(pdf: &Pdf, node: &Object, visited: &mut HashSet<ObjectId>, scripts: &mut Vec<DocumentScript>, depth: usize) {
    if depth > 32 {
        return;
    }
    if let Object::Reference(id) = node {
        if !visited.insert(*id) {
            return;
        }
    }
    let Ok(node) = pdf.resolve(node).as_dict() else { return };
    if let Some(pairs) = node.get(b"Names").ok().and_then(|names| pdf.resolve(names).as_array().ok()) {
        for pair in pairs.chunks(2) {
            let [name, action] = pair else { continue };
            let source = javascript_of_action(pdf, action);
            if !source.trim().is_empty() {
                scripts.push(DocumentScript { name: object_to_text(pdf.resolve(name)).unwrap_or_default(), source });
            }
        }
    }
    if let Some(kids) = node.get(b"Kids").ok().and_then(|kids| pdf.resolve(kids).as_array().ok()) {
        for kid in kids {
            collect_name_tree(pdf, kid, visited, scripts, depth + 1);
        }
    }
}

/// The fields with calculations in the order `/AcroForm /CO` gives, then
/// any it leaves out in document order (Acrobat would skip those, but a
/// form that forgot one still means it to calculate).
fn calculation_order(pdf: &Pdf, fields: &[ScriptedField]) -> Vec<usize> {
    let calculating = |index: &usize| fields[*index].scripts.calculate.is_some();
    let listed: Vec<ObjectId> = pdf
        .doc
        .catalog()
        .ok()
        .and_then(|catalog| catalog.get(b"AcroForm").ok())
        .and_then(|form| pdf.resolve(form).as_dict().ok())
        .and_then(|form| form.get(b"CO").ok())
        .and_then(|order| pdf.resolve(order).as_array().ok())
        .map(|order| order.iter().filter_map(|entry| entry.as_reference().ok()).collect())
        .unwrap_or_default();
    let mut order: Vec<usize> = Vec::new();
    for id in listed {
        if let Some(index) = fields.iter().position(|field| field.info.id == Some(id)) {
            if calculating(&index) && !order.contains(&index) {
                order.push(index);
            }
        }
    }
    for index in 0..fields.len() {
        if calculating(&index) && !order.contains(&index) {
            order.push(index);
        }
    }
    order
}

/// Where the document keeps scripts that Quintess never runs.
fn scripts_never_run(pdf: &Pdf) -> Vec<String> {
    let mut places = Vec::new();
    let is_script = |action: &Object| !javascript_of_action(pdf, action).trim().is_empty();
    let scripted_actions = |dictionary: &Dictionary, keys: &[&[u8]]| -> Vec<String> {
        let Some(actions) = dictionary.get(b"AA").ok().and_then(|aa| pdf.resolve(aa).as_dict().ok()) else { return Vec::new() };
        actions.iter().filter(|(key, action)| (keys.is_empty() || keys.contains(&key.as_slice())) && is_script(action)).map(|(key, _)| String::from_utf8_lossy(key).into_owned()).collect()
    };
    if let Ok(catalog) = pdf.doc.catalog() {
        if catalog.get(b"OpenAction").is_ok_and(is_script) {
            places.push("the document's open action".to_string());
        }
        for trigger in scripted_actions(catalog, &[]) {
            places.push(format!("the document's {trigger} action (close, save or print)"));
        }
    }
    for (page_index, page_id) in pdf.page_ids().into_iter().enumerate() {
        let page_number = page_index + 1;
        let Ok(page) = pdf.doc.get_dictionary(page_id) else { continue };
        for trigger in scripted_actions(page, &[]) {
            places.push(format!("page {page_number}: the page's {} action", if trigger == "O" { "open" } else { "close" }));
        }
        let annotations = page.get(b"Annots").ok().and_then(|annots| pdf.resolve(annots).as_array().ok()).cloned().unwrap_or_default();
        for annotation in annotations {
            let Ok(annotation) = pdf.resolve(&annotation).as_dict() else { continue };
            let subtype = annotation.get(b"Subtype").ok().and_then(|s| s.as_name().ok()).map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_default();
            if annotation.get(b"A").is_ok_and(is_script) {
                places.push(format!("page {page_number}: a {} action", if subtype == "Link" { "link".to_string() } else { format!("{} annotation", subtype.to_lowercase()) }));
            }
            // Mouse and focus triggers on widgets (and anything on other
            // annotations); the field triggers K, V, C and F are run.
            let pointer_triggers: &[&[u8]] = &[b"E", b"X", b"D", b"U", b"Fo", b"Bl", b"PO", b"PC", b"PV", b"PI"];
            for trigger in scripted_actions(annotation, pointer_triggers) {
                places.push(format!("page {page_number}: a {} {trigger} action", if subtype == "Widget" { "field's" } else { "comment's" }));
            }
        }
    }
    places
}

// ---------------------------------------------------------------------------
// Running scripts on their own thread.

#[derive(Clone, Debug)]
enum Request {
    Apply { target: usize, value: String },
    Recalculate,
    Keystroke { target: usize, current: String, change: String, selection_start: usize, selection_end: usize },
}

/// What the script thread sends back.
#[derive(Clone, Debug, Default)]
struct ScriptReport {
    /// Every field's state after the calculations, in form order (empty
    /// when the scripts never got that far).
    final_states: Vec<FieldState>,
    /// The text each field's format script produced, where one ran.
    shown: Vec<Option<String>>,
    alerts: Vec<String>,
    errors: Vec<String>,
    rejected: Option<RejectedValue>,
    /// For a keystroke: whether it is allowed, and the change to insert.
    keystroke: Option<(bool, String)>,
}

/// A field's state as the scripts left it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FieldState {
    value: String,
    display: u8,
    readonly: bool,
    required: bool,
    /// The color a script chose for the text, as RGB.
    text_color: Option<[f32; 3]>,
    /// The list's new choices, when a script replaced them.
    items: Option<Vec<(String, String)>>,
}

/// Runs a request on a thread of its own and waits for the report, giving
/// up if the scripts outlast the time limit.
fn run_scripts(form: FormSnapshot, request: Request) -> Result<ScriptReport> {
    let (sender, receiver) = mpsc::channel();
    let deadline = Instant::now() + SCRIPT_TIME_LIMIT;
    std::thread::Builder::new().name("form scripts".into()).stack_size(SCRIPT_THREAD_STACK_BYTES).spawn(move || {
        let report = run_request(&form, &request, deadline);
        // The caller may have stopped waiting; then nobody needs the report.
        let _ = sender.send(report);
    })?;
    match receiver.recv_timeout(SCRIPT_TIME_LIMIT + SCRIPT_THREAD_GRACE) {
        Ok(report) => Ok(report),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(anyhow!("this form's scripts ran too long and were stopped")),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow!("this form's scripts could not be run")),
    }
}

/// The details of one event, as the prelude's beginEvent reads them.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EventDetails<'a> {
    name: &'a str,
    target: usize,
    source: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<&'a str>,
    change: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    sel_start: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sel_end: Option<usize>,
    will_commit: bool,
}

impl<'a> EventDetails<'a> {
    fn new(name: &'a str, target: usize) -> EventDetails<'a> {
        EventDetails { name, target, source: None, value: None, change: "", sel_start: None, sel_end: None, will_commit: false }
    }
}

/// What a script made of an event.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventResult {
    accepted: bool,
    value: String,
    value_changed: bool,
    change: String,
}

fn run_request(form: &FormSnapshot, request: &Request, deadline: Instant) -> ScriptReport {
    let mut report = ScriptReport::default();
    let mut session = match ScriptSession::start(form, deadline, &mut report) {
        Ok(session) => session,
        Err(problem) => {
            report.errors.push(problem);
            return report;
        }
    };
    match request {
        Request::Keystroke { target, current, change, selection_start, selection_end } => {
            let field = &form.fields[*target];
            let details = EventDetails { value: Some(current), change, sel_start: Some(*selection_start), sel_end: Some(*selection_end), ..EventDetails::new("Keystroke", *target) };
            report.keystroke = Some(match session.run_event(&details, field.scripts.keystroke.as_deref().unwrap_or("")) {
                Ok(result) => (result.accepted, result.change),
                Err(failure) => {
                    report.errors.push(describe_failure(field, "keystroke", &failure));
                    (true, change.clone())
                }
            });
            report.alerts.extend(session.take_alerts());
        }
        Request::Apply { target, value } => {
            let field = &form.fields[*target];
            let mut value = value.clone();
            // The committing keystroke, then validation; either may refuse.
            for (event_name, kind, script) in [("Keystroke", "keystroke", &field.scripts.keystroke), ("Validate", "validate", &field.scripts.validate)] {
                let Some(script) = script else { continue };
                let details = EventDetails { value: Some(&value), will_commit: true, ..EventDetails::new(event_name, *target) };
                match session.run_event(&details, script) {
                    Ok(result) if !result.accepted => {
                        let alerts = session.take_alerts();
                        let message = if alerts.is_empty() { format!("The form does not accept \"{value}\" for {}.", field.info.name) } else { alerts.join("\n") };
                        report.alerts.extend(alerts);
                        report.rejected = Some(RejectedValue { field: field.info.name.clone(), value, message });
                        return report;
                    }
                    Ok(result) => value = result.value,
                    Err(failure) => report.errors.push(describe_failure(field, kind, &failure)),
                }
            }
            if let Err(failure) = session.store(*target, &value) {
                report.errors.push(failure.to_string());
                return report;
            }
            session.run_calculations(form, Some(*target), &mut report);
            // Format the field typed into and every field whose value changed.
            session.finish(form, |index, state| index == *target || state.value != form.fields[index].info.value, &mut report);
        }
        Request::Recalculate => {
            session.run_calculations(form, None, &mut report);
            session.finish(form, |_, _| true, &mut report);
        }
    }
    report
}

fn describe_failure(field: &ScriptedField, kind: &str, failure: &ScriptFailure) -> String {
    format!("{} ({kind}): {failure}", field.info.name)
}

/// Why a script did not finish.
#[derive(Debug)]
enum ScriptFailure {
    /// It threw an error or would not parse.
    Error(String),
    /// It reached a limit on loops, recursion or time.
    Stopped,
}

impl std::fmt::Display for ScriptFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScriptFailure::Error(message) => formatter.write_str(message),
            ScriptFailure::Stopped => formatter.write_str("stopped because it ran too long"),
        }
    }
}

/// The form fields seen by scripts, as the prelude's load reads them.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FieldForScripts<'a> {
    name: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    value: &'a str,
    items: &'a [(String, String)],
    export_values: &'a [String],
    readonly: bool,
    required: bool,
    display: u8,
    multiline: bool,
    char_limit: Option<u32>,
    tooltip: &'a str,
    /// 0-based, as Acrobat counts pages.
    page: i64,
    rect: [f32; 4],
    editable: bool,
    default_value: &'a str,
}

/// Acrobat's name for a field type.
fn acrobat_type_name(field_type: &FieldType) -> &'static str {
    match field_type {
        FieldType::Text => "text",
        FieldType::CheckBox => "checkbox",
        FieldType::RadioGroup => "radiobutton",
        FieldType::PushButton => "button",
        FieldType::ComboBox => "combobox",
        FieldType::ListBox => "listbox",
        FieldType::Signature => "signature",
        FieldType::Unknown => "unknown",
    }
}

/// One JavaScript engine loaded with the prelude and the form, living on
/// the script thread for one request.
struct ScriptSession {
    context: Context,
    deadline: Instant,
    /// Set once a script hit the time limit mid-run; the engine is then
    /// left mid-script and must not run anything more.
    abandoned: bool,
}

impl ScriptSession {
    fn start(form: &FormSnapshot, deadline: Instant, report: &mut ScriptReport) -> Result<ScriptSession, String> {
        let mut context = Context::default();
        context.runtime_limits_mut().set_loop_iteration_limit(LOOP_ITERATION_LIMIT);
        context.runtime_limits_mut().set_recursion_limit(RECURSION_LIMIT);
        let mut session = ScriptSession { context, deadline, abandoned: false };
        session.evaluate(PRELUDE).map_err(|failure| format!("the form script support did not load: {failure}"))?;
        let fields: Vec<FieldForScripts> = form
            .fields
            .iter()
            .map(|field| FieldForScripts {
                name: &field.info.name,
                kind: acrobat_type_name(&field.info.field_type),
                value: &field.info.value,
                items: &field.items,
                export_values: &field.export_values,
                readonly: field.info.read_only,
                required: field.info.required,
                display: field.display,
                multiline: field.info.multiline,
                char_limit: field.info.max_length,
                tooltip: &field.info.tooltip,
                page: field.info.widgets.first().map(|widget| widget.0 as i64 - 1).unwrap_or(0),
                rect: field.info.widgets.first().map(|widget| widget.1).unwrap_or_default(),
                editable: field.editable,
                default_value: &field.default_value,
            })
            .collect();
        let data = serde_json::json!({ "fields": fields, "pageCount": form.page_count, "platform": if cfg!(windows) { "WIN" } else { "UNIX" } });
        session.evaluate(&format!("__quintess.load({data});")).map_err(|failure| format!("the form's fields did not load: {failure}"))?;
        session.evaluate("__quintess.loadingDocumentScripts = true;").map_err(|failure| failure.to_string())?;
        for script in &form.document_scripts {
            if let Err(failure) = session.evaluate(&script.source) {
                report.errors.push(format!("document script {}: {failure}", script.name));
                if session.abandoned {
                    return Err("the document's scripts ran too long and were stopped".into());
                }
            }
        }
        session.evaluate("__quintess.loadingDocumentScripts = false;").map_err(|failure| failure.to_string())?;
        // Messages from scripts run on opening aren't about anything typed.
        session.take_alerts();
        Ok(session)
    }

    /// Runs source text and returns its result as text.
    fn evaluate(&mut self, source: &str) -> Result<String, ScriptFailure> {
        if self.abandoned {
            return Err(ScriptFailure::Stopped);
        }
        let script = Script::parse(Source::from_bytes(source.as_bytes()), None, &mut self.context).map_err(|error| ScriptFailure::Error(error.to_string()))?;
        let Some(result) = run_until_deadline(script.evaluate_async_with_budget(&mut self.context, INSTRUCTIONS_BETWEEN_TIME_CHECKS), self.deadline) else {
            self.abandoned = true;
            return Err(ScriptFailure::Stopped);
        };
        match result {
            Ok(value) => value.to_string(&mut self.context).map(|text| text.to_std_string_escaped()).map_err(|error| ScriptFailure::Error(error.to_string())),
            // Engine errors are the loop and recursion limits (scripts
            // can't catch them).
            Err(error) if error.as_engine().is_some() => Err(ScriptFailure::Stopped),
            Err(error) => Err(ScriptFailure::Error(error.to_string())),
        }
    }

    /// Runs one field script for an event and reads what it decided.
    fn run_event(&mut self, details: &EventDetails, script: &str) -> Result<EventResult, ScriptFailure> {
        let details = serde_json::to_string(details).expect("event details serialize");
        self.evaluate(&format!("__quintess.beginEvent({details});"))?;
        // Each script runs as a function body, so its variables stay its
        // own, as in Acrobat. A plain call of a non-strict function makes
        // `this` the global object, where getField and the other document
        // methods live. (Calling it through Function.prototype.call would
        // run it inside a built-in, out of reach of the time limit.)
        let ran = self.evaluate(&format!("(function () {{\n{script}\n}})();"));
        let finished = self.evaluate("__quintess.finishEvent()");
        ran?;
        let finished = finished?;
        serde_json::from_str(&finished).map_err(|error| ScriptFailure::Error(error.to_string()))
    }

    fn store(&mut self, target: usize, value: &str) -> Result<(), ScriptFailure> {
        let value = serde_json::to_string(value).expect("text serializes");
        self.evaluate(&format!("__quintess.store(__quintess.fields[{target}], {value});")).map(|_| ())
    }

    fn take_alerts(&mut self) -> Vec<String> {
        self.evaluate("__quintess.takeAlerts()").ok().and_then(|alerts| serde_json::from_str(&alerts).ok()).unwrap_or_default()
    }

    fn export_state(&mut self) -> Result<Vec<FieldState>, ScriptFailure> {
        let state = self.evaluate("__quintess.exportState()")?;
        serde_json::from_str(&state).map_err(|error| ScriptFailure::Error(error.to_string()))
    }

    /// Runs every calculation once, in order. `source` is the field whose
    /// change set them off (event.source), if any.
    fn run_calculations(&mut self, form: &FormSnapshot, source: Option<usize>, report: &mut ScriptReport) {
        // A script stopped by the time limit leaves the engine unusable, so
        // the values are read back after every calculation: what was worked
        // out before the stop is kept.
        self.record_state(report);
        for &index in &form.calculation_order {
            let field = &form.fields[index];
            let Some(script) = &field.scripts.calculate else { continue };
            let details = EventDetails { source, ..EventDetails::new("Calculate", index) };
            match self.run_event(&details, script) {
                Ok(result) if result.accepted && result.value_changed => {
                    if let Err(failure) = self.store(index, &result.value) {
                        report.errors.push(describe_failure(field, "calculate", &failure));
                    }
                }
                Ok(_) => {}
                Err(failure) => report.errors.push(describe_failure(field, "calculate", &failure)),
            }
            if self.abandoned {
                return;
            }
            self.record_state(report);
        }
        report.alerts.extend(self.take_alerts());
    }

    /// Reads every field's state back into the report.
    fn record_state(&mut self, report: &mut ScriptReport) {
        match self.export_state() {
            Ok(states) => report.final_states = states,
            Err(failure) if !self.abandoned => report.errors.push(format!("the form's values could not be read back: {failure}")),
            Err(_) => {}
        }
    }

    /// Runs the format scripts of the fields `needs_formatting` picks, to
    /// work out the text they show.
    fn finish(&mut self, form: &FormSnapshot, needs_formatting: impl Fn(usize, &FieldState) -> bool, report: &mut ScriptReport) {
        if self.abandoned || report.final_states.len() != form.fields.len() {
            return;
        }
        report.shown = vec![None; form.fields.len()];
        for (index, field) in form.fields.iter().enumerate() {
            let Some(script) = &field.scripts.format else { continue };
            if !needs_formatting(index, &report.final_states[index]) {
                continue;
            }
            match self.run_event(&EventDetails::new("Format", index), script) {
                Ok(result) => report.shown[index] = Some(result.value),
                Err(failure) => report.errors.push(describe_failure(field, "format", &failure)),
            }
        }
        // Format scripts may color their text (red for negative amounts);
        // the values they leave are not stored, as in Acrobat.
        if let Ok(states) = self.export_state() {
            for (index, state) in states.into_iter().enumerate() {
                if let Some(final_state) = report.final_states.get_mut(index) {
                    final_state.text_color = state.text_color;
                }
            }
        }
        report.alerts.extend(self.take_alerts());
    }
}

/// Polls a script's future until it finishes or the deadline passes. Boa
/// yields every so many instructions, which is when the clock is checked.
fn run_until_deadline<F: Future>(future: F, deadline: Instant) -> Option<F::Output> {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return Some(output);
        }
        if Instant::now() >= deadline {
            return None;
        }
    }
}

// ---------------------------------------------------------------------------
// Writing the results into the PDF.

fn apply_report(pdf: &mut Pdf, form: &FormSnapshot, report: &ScriptReport, outcome: &mut ScriptOutcome) -> Result<()> {
    for (index, field) in form.fields.iter().enumerate() {
        let (Some(state), Some(field_id)) = (report.final_states.get(index), field.info.id) else { continue };
        let value_changed = state.value != field.info.value;
        let shown = report.shown.get(index).cloned().flatten();
        let mut rewritten = false;
        match field.info.field_type {
            FieldType::Text | FieldType::ComboBox | FieldType::ListBox => {
                let shown_differs = shown.as_ref().is_some_and(|text| text != &state.value);
                if value_changed || shown_differs || state.text_color.is_some() {
                    forms::set_text_value(pdf, field_id, &state.value, shown.as_deref().unwrap_or(&state.value), state.text_color)?;
                    rewritten = value_changed || shown_differs;
                }
            }
            FieldType::CheckBox | FieldType::RadioGroup if value_changed => {
                forms::fill_fields(pdf, &BTreeMap::from([(field.info.name.clone(), state.value.clone())]))?;
                rewritten = true;
            }
            _ => {}
        }
        if state.readonly != field.info.read_only || state.required != field.info.required {
            let field_dictionary = pdf.doc.get_dictionary(field_id)?.clone();
            let mut flags = forms::inherited(pdf, &field_dictionary, b"Ff").and_then(|flags| flags.as_i64().ok()).unwrap_or(0);
            flags = if state.readonly { flags | FIELD_FLAG_READ_ONLY } else { flags & !FIELD_FLAG_READ_ONLY };
            flags = if state.required { flags | FIELD_FLAG_REQUIRED } else { flags & !FIELD_FLAG_REQUIRED };
            pdf.doc.get_dictionary_mut(field_id)?.set("Ff", flags);
        }
        if state.display != field.display {
            for widget in forms::widget_ids(pdf, field_id) {
                let widget_dictionary = pdf.doc.get_dictionary_mut(widget)?;
                let flags = widget_dictionary.get(b"F").ok().and_then(|flags| flags.as_i64().ok()).unwrap_or(0);
                widget_dictionary.set("F", annotation_flags_for_display(flags, state.display));
            }
        }
        if let Some(items) = &state.items {
            let options = items.iter().map(|(shown, export)| if shown == export { text_string(shown) } else { Object::Array(vec![text_string(export), text_string(shown)]) }).collect();
            pdf.doc.get_dictionary_mut(field_id)?.set("Opt", Object::Array(options));
        }
        if rewritten {
            outcome.changed_fields.push(field.info.name.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs JavaScript after the prelude, with no form loaded.
    fn evaluate(source: &str) -> String {
        let mut report = ScriptReport::default();
        let mut session = ScriptSession::start(&FormSnapshot::default(), Instant::now() + Duration::from_secs(20), &mut report).expect("the prelude loads");
        session.evaluate(source).unwrap_or_else(|failure| panic!("{source}: {failure}"))
    }

    /// What a format or keystroke built-in leaves in event.value for `value`.
    fn formatted(value: &str, call: &str) -> String {
        evaluate(&format!("event = {{ value: {value}, target: {{ name: 'amount' }}, rc: true, willCommit: false, change: '', selStart: -1, selEnd: -1 }}; {call}; event.value"))
    }

    #[test]
    fn number_formats_follow_acrobats_separator_and_negative_styles() {
        let styles = ["1,234.56", "1234.56", "1.234,56", "1234,56", "1'234.56"];
        for (style, expected) in styles.iter().enumerate() {
            assert_eq!(formatted("1234.56", &format!("AFNumber_Format(2, {style}, 0, 0, '', true)")), *expected, "separator style {style}");
        }
        assert_eq!(formatted("'1234.5'", "AFNumber_Format(2, 0, 0, 0, '$', true)"), "$1,234.50");
        assert_eq!(formatted("-1234.5", "AFNumber_Format(2, 0, 0, 0, '$', true)"), "-$1,234.50");
        assert_eq!(formatted("-1234.5", "AFNumber_Format(2, 0, 2, 0, '$', true)"), "($1,234.50)");
        assert_eq!(formatted("-1234.5", "AFNumber_Format(2, 2, 0, 0, ' €', false)"), "-1.234,50 €");
        assert_eq!(formatted("-5", "AFNumber_Format(0, 0, 1, 0, '', true)"), "5", "the red style drops the sign");
        assert_eq!(evaluate("event = { value: -5, target: getField('none') || { name: 'x' } }; AFNumber_Format(0, 0, 1, 0, '', true); JSON.stringify(event.target.textColor)"), "[\"RGB\",1,0,0]");
        assert_eq!(formatted("''", "AFNumber_Format(2, 0, 0, 0, '$', true)"), "", "an empty field stays empty");
        assert_eq!(formatted("0.256", "AFPercent_Format(1, 0)"), "25.6%");
    }

    #[test]
    fn util_printf_printd_scand_and_printx() {
        assert_eq!(evaluate("util.printf('%,0.2f', 1234567.891)"), "1,234,567.89");
        assert_eq!(evaluate("util.printf('%05d|%+d|%x', 42, 7, 255)"), "00042|+7|ff");
        assert_eq!(evaluate("util.printf('%s and %s, 100%%', 'a', 1)"), "a and 1, 100%");
        assert_eq!(evaluate("util.printd('yyyy-mm-dd HH:MM tt', new Date(2026, 2, 4, 15, 7))"), "2026-03-04 15:07 pm");
        assert_eq!(evaluate("util.printd('dddd, mmm d, yy h:MM', new Date(2026, 2, 4, 15, 7))"), "Wednesday, Mar 4, 26 3:07");
        assert_eq!(evaluate("util.printd('yyyy-mm-dd', util.scand('mm/dd/yyyy', '03/04/2026'))"), "2026-03-04");
        assert_eq!(evaluate("util.printd('yyyy-mm-dd', util.scand('mmmm d, yyyy', 'march 4, 2026'))"), "2026-03-04");
        assert_eq!(evaluate("util.printd('HH:MM', util.scand('h:MM tt', '2:30 pm'))"), "14:30");
        assert_eq!(evaluate("String(util.scand('mm/dd/yyyy', '02/30/2026'))"), "null", "February 30th doesn't exist");
        assert_eq!(evaluate("util.printx('(999) 999-9999', '555.123.4567')"), "(555) 123-4567");
        assert_eq!(evaluate("util.printx('>AAA-999', 'ab c123')"), "ABC-123");
    }

    #[test]
    fn dates_times_and_special_formats() {
        assert_eq!(formatted("'3/4/2026'", "AFDate_FormatEx('mmmm d, yyyy')"), "March 4, 2026");
        assert_eq!(formatted("'2026-03-04'", "AFDate_FormatEx('mm/dd/yyyy')"), "03/04/2026", "a four-digit first number is the year");
        assert_eq!(formatted("'March 4, 2026'", "AFDate_Format(2)"), "03/04/26");
        assert_eq!(formatted("'14:05'", "AFTime_Format(1)"), "2:05 pm");
        assert_eq!(formatted("'123456789'", "AFSpecial_Format(1)"), "12345-6789");
        assert_eq!(formatted("'123456789'", "AFSpecial_Format(3)"), "123-45-6789");
        assert_eq!(formatted("'5551234'", "AFSpecial_Format(2)"), "555-1234");
        assert_eq!(evaluate("String(AFMakeNumber('1234,5')) + ' ' + String(AFMakeNumber('abc'))"), "1234.5 null");
    }

    #[test]
    fn masks_put_in_their_own_punctuation_while_typing() {
        let typed = |current: &str, change: &str| evaluate(&format!("event = {{ value: '{current}', change: '{change}', selStart: {0}, selEnd: {0}, willCommit: false, rc: true, target: {{ name: 'phone' }} }}; AFSpecial_KeystrokeEx('999-9999'); event.rc + ' ' + event.change", current.len()));
        assert_eq!(typed("555", "1"), "true -1");
        assert_eq!(typed("555-", "1"), "true 1");
        assert_eq!(typed("555", "x"), "false x");
        assert_eq!(typed("555-1234", "5"), "false 5", "the mask is full");
    }

    #[test]
    fn display_constants_round_trip_through_annotation_flags() {
        for display in 0..=3 {
            assert_eq!(display_from_annotation_flags(annotation_flags_for_display(4, display)), display);
        }
        assert_eq!(annotation_flags_for_display(4 | 128, 0) & 128, 128, "unrelated flags (locked) stay");
    }
}
